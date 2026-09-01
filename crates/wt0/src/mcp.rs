//! Model Context Protocol transport for the Worktree Zero lifecycle.
//!
//! `wt0 mcp serve` speaks MCP over stdio (newline-delimited JSON-RPC 2.0) so
//! agent hosts — Claude Code, Codex, Gemini CLI, Cursor, OpenClaw, NanoClaw,
//! Hermes, Grok Bot, and any other MCP client — can call the same versioned
//! lifecycle the JSON CLI ships. Every tool call re-invokes this executable
//! with `--json`, so MCP is a transport over the one contract, never a second
//! implementation that could drift from it.

use anyhow::{Context, Result};
use clap::{Args, Subcommand};
use serde_json::{json, Map, Value};
use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::Command;

/// Spec revisions this server can negotiate, newest first. An unknown client
/// revision is answered with the newest supported one, per the specification.
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] =
    &["2026-07-28", "2025-06-18", "2025-03-26", "2024-11-05"];

#[derive(Args)]
pub struct Mcp {
    #[command(subcommand)]
    pub command: McpCommand,
}

#[derive(Subcommand)]
pub enum McpCommand {
    /// Serve the lifecycle over stdio for MCP clients.
    Serve,
}

pub fn run(args: Mcp) -> Result<()> {
    match args.command {
        McpCommand::Serve => serve(),
    }
}

fn serve() -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line.context("read MCP stdio message")?;
        if line.trim().is_empty() {
            continue;
        }
        let Some(response) = handle_message(&line) else {
            continue;
        };
        let mut out = stdout.lock();
        serde_json::to_writer(&mut out, &response).context("encode MCP response")?;
        out.write_all(b"\n").context("write MCP response")?;
        out.flush().context("flush MCP response")?;
    }
    Ok(())
}

/// Handle one JSON-RPC message. Returns the response to send, or `None` for
/// notifications and other messages that must not be answered.
fn handle_message(line: &str) -> Option<Value> {
    let message: Value = match serde_json::from_str(line) {
        Ok(message) => message,
        Err(error) => {
            return Some(error_response(
                Value::Null,
                -32700,
                &format!("parse error: {error}"),
            ))
        }
    };
    let id = message.get("id").cloned();
    let method = message.get("method").and_then(Value::as_str);
    let params = message.get("params").cloned().unwrap_or(json!({}));

    let (id, method) = match (id, method) {
        // A notification: initialized, cancelled, progress — nothing to answer.
        (None, Some(_)) => return None,
        (Some(id), Some(method)) => (id, method.to_owned()),
        (id, None) => {
            return Some(error_response(
                id.unwrap_or(Value::Null),
                -32600,
                "invalid request: missing method",
            ))
        }
    };

    let result = match method.as_str() {
        "initialize" => Ok(initialize_result(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => call_tool(&params),
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => error_response(id, code, &message),
    })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

fn initialize_result(params: &Value) -> Value {
    let requested = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let negotiated = SUPPORTED_PROTOCOL_VERSIONS
        .iter()
        .find(|version| **version == requested)
        .unwrap_or(&SUPPORTED_PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": negotiated,
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": "worktree-zero",
            "title": "Worktree Zero",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "instructions": "One guarded lifecycle for thin agent worktrees: discover \
            capabilities first, create copy-on-write checkouts, refresh heartbeats \
            for long work, and clean up with dry-run-first gc. Refusals are safety \
            guards — surface their reason to a human instead of working around them. \
            Pass `repo` (an absolute path) to address a repository other than the \
            server's working directory.",
    })
}

/// One lifecycle tool: its MCP metadata plus how to translate arguments into
/// the equivalent `wt0 --json` invocation.
struct Tool {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    /// (CLI subcommand argv fragments, JSON Schema properties, required keys)
    properties: Value,
    required: &'static [&'static str],
}

fn tools() -> Vec<Tool> {
    let repo = json!({
        "type": "string",
        "description": "Absolute path of the repository or worktree to operate in. Defaults to the server's working directory.",
    });
    vec![
        Tool {
            name: "capabilities",
            title: "Discover capabilities",
            description: "Report the storage backend, detected package managers, generated-state tools, and protocol surfaces of this installation. Call before creating runtimes.",
            properties: json!({ "repo": repo }),
            required: &[],
        },
        Tool {
            name: "doctor",
            title: "Inspect runtime readiness",
            description: "Inspect dependency sharing and generated runtime storage. Exits non-ready when stale layouts or over-budget generated state need attention.",
            properties: json!({ "repo": repo }),
            required: &[],
        },
        Tool {
            name: "create_worktree",
            title: "Create a thin worktree",
            description: "Create a real Git linked worktree populated with copy-on-write clones where supported. Returns the worktree path, populate mode, and ownership receipt.",
            properties: json!({
                "repo": repo,
                "branch": { "type": "string", "description": "New branch name, e.g. agent/fix-checkout." },
                "path": { "type": "string", "description": "Worktree path. Defaults to .git/wt0/worktrees/<branch>." },
                "base": { "type": "string", "description": "Commit-ish to start from. Defaults to HEAD." },
                "require_cow": { "type": "boolean", "description": "Fail instead of falling back to a plain checkout when copy-on-write is unavailable." },
                "ephemeral": { "type": "boolean", "description": "Mark the worktree for automatic gc selection." },
            }),
            required: &["branch"],
        },
        Tool {
            name: "list_worktrees",
            title: "List worktrees",
            description: "List linked worktrees from Git's native registry.",
            properties: json!({ "repo": repo }),
            required: &[],
        },
        Tool {
            name: "remove_worktree",
            title: "Remove a worktree",
            description: "Remove a linked worktree by path or branch name. Refuses dirty worktrees unless work is committed first.",
            properties: json!({
                "repo": repo,
                "target": { "type": "string", "description": "Worktree path or branch name." },
                "commit": { "type": "boolean", "description": "Commit all changes before removing." },
                "message": { "type": "string", "description": "Commit message when commit is true." },
                "delete_branch": { "type": "boolean", "description": "Delete the worktree's branch too; unmerged branches are refused." },
            }),
            required: &["target"],
        },
        Tool {
            name: "heartbeat",
            title: "Refresh the ownership lease",
            description: "Refresh the ownership lease for a running agent worktree so gc treats it as active.",
            properties: json!({
                "repo": repo,
                "target": { "type": "string", "description": "Worktree path or branch. Defaults to the working directory's worktree." },
            }),
            required: &[],
        },
        Tool {
            name: "gc",
            title: "Garbage-collect runtimes",
            description: "Reap eligible abandoned worktrees. Dry-run by default; set apply to true only after reviewing the dry-run receipt. Never discards dirty or unowned work.",
            properties: json!({
                "repo": repo,
                "apply": { "type": "boolean", "description": "Apply the reported garbage collection. Dry-run is the default." },
                "ephemeral": { "type": "boolean", "description": "Only reap worktrees created as ephemeral." },
                "prefix": { "type": "string", "description": "Only reap worktrees whose branch starts with this prefix." },
                "older_than": { "type": "string", "description": "Reap worktrees idle at least this long (90s, 30m, 24h, 7d). Default 24h." },
                "delete_branches": { "type": "boolean", "description": "Delete each reaped worktree's branch; unmerged branches are retained." },
                "allow_generated": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Additional reviewed relative paths gc may treat as generated state.",
                },
            }),
            required: &[],
        },
        Tool {
            name: "prune",
            title: "Prune stale state",
            description: "Prune stale Git registrations, retire orphaned owned generated runtimes, and remove old cached baselines.",
            properties: json!({
                "repo": repo,
                "all": { "type": "boolean", "description": "Also delete every cached baseline, including recently used entries." },
            }),
            required: &[],
        },
        Tool {
            name: "repair",
            title: "Repair overlay worktrees",
            description: "Remount interrupted Linux overlay-backed worktrees after a reboot or crash.",
            properties: json!({ "repo": repo }),
            required: &[],
        },
        Tool {
            name: "prepare",
            title: "Prepare dependencies",
            description: "Prepare package-manager state for a thin runtime: verify the manager, then attach a private copy-on-write prepared environment. Dry-run unless apply is true.",
            properties: json!({
                "repo": repo,
                "apply": { "type": "boolean", "description": "Apply the reported preparation. Dry-run is the default." },
            }),
            required: &[],
        },
        Tool {
            name: "migrate",
            title: "Audit or migrate existing worktrees",
            description: "Audit existing linked worktrees and, when safety preconditions pass, share identical tracked files with the canonical baseline. Dry-run unless apply is true.",
            properties: json!({
                "repo": repo,
                "all": { "type": "boolean", "description": "Inspect every linked worktree registered to this repository." },
                "apply": { "type": "boolean", "description": "Apply only actions whose safety preconditions pass." },
                "baseline": { "type": "string", "description": "Canonical source ref whose identical tracked files should be shared." },
                "source_only": { "type": "boolean", "description": "Migrate tracked source even when a dependency adapter is unavailable." },
                "adopt": { "type": "boolean", "description": "Record Worktree Zero ownership after every selected migration succeeds. Requires apply." },
            }),
            required: &[],
        },
    ]
}

fn tool_definitions() -> Vec<Value> {
    tools()
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "title": tool.title,
                "description": tool.description,
                "inputSchema": {
                    "type": "object",
                    "properties": tool.properties,
                    "required": tool.required,
                    "additionalProperties": false,
                },
            })
        })
        .collect()
}

fn call_tool(params: &Value) -> Result<Value, (i64, String)> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or((-32602, "tools/call requires a tool name".to_owned()))?;
    let arguments = match params.get("arguments") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(map)) => map.clone(),
        Some(_) => return Err((-32602, "tool arguments must be an object".to_owned())),
    };
    if tools().iter().all(|tool| tool.name != name) {
        return Err((-32602, format!("unknown tool: {name}")));
    }
    let (argv, repo) = build_argv(name, &arguments).map_err(|error| (-32602, error))?;
    Ok(execute(argv, repo))
}

/// Translate validated tool arguments into a `wt0 --json …` argv plus the
/// working directory to run it in.
fn build_argv(
    name: &str,
    arguments: &Map<String, Value>,
) -> Result<(Vec<OsString>, Option<PathBuf>), String> {
    let text = |key: &str| -> Result<Option<String>, String> {
        match arguments.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(value)) => Ok(Some(value.clone())),
            Some(_) => Err(format!("argument '{key}' must be a string")),
        }
    };
    let flag = |key: &str| -> Result<bool, String> {
        match arguments.get(key) {
            None | Some(Value::Null) => Ok(false),
            Some(Value::Bool(value)) => Ok(*value),
            Some(_) => Err(format!("argument '{key}' must be a boolean")),
        }
    };
    let repo = match text("repo")? {
        None => None,
        Some(path) => {
            let path = PathBuf::from(path);
            if !path.is_absolute() {
                return Err("argument 'repo' must be an absolute path".to_owned());
            }
            Some(path)
        }
    };
    let require = |key: &str| -> Result<String, String> {
        text(key)?.ok_or_else(|| format!("argument '{key}' is required"))
    };

    let mut argv: Vec<OsString> = vec![OsString::from("--json")];
    match name {
        "capabilities" => argv.push("capabilities".into()),
        "doctor" => argv.push("doctor".into()),
        "list_worktrees" => argv.push("list".into()),
        "repair" => argv.push("repair".into()),
        "create_worktree" => {
            argv.push("create".into());
            argv.push(require("branch")?.into());
            if let Some(path) = text("path")? {
                argv.push("--path".into());
                argv.push(path.into());
            }
            if let Some(base) = text("base")? {
                argv.push("--base".into());
                argv.push(base.into());
            }
            if flag("require_cow")? {
                argv.push("--require-cow".into());
            }
            if flag("ephemeral")? {
                argv.push("--ephemeral".into());
            }
        }
        "remove_worktree" => {
            argv.push("remove".into());
            argv.push(require("target")?.into());
            if flag("commit")? {
                argv.push("--commit".into());
            }
            if let Some(message) = text("message")? {
                argv.push("--message".into());
                argv.push(message.into());
            }
            if flag("delete_branch")? {
                argv.push("--delete-branch".into());
            }
        }
        "heartbeat" => {
            argv.push("heartbeat".into());
            if let Some(target) = text("target")? {
                argv.push(target.into());
            }
        }
        "gc" => {
            argv.push("gc".into());
            if flag("apply")? {
                argv.push("--apply".into());
            }
            if flag("ephemeral")? {
                argv.push("--ephemeral".into());
            }
            if let Some(prefix) = text("prefix")? {
                argv.push("--prefix".into());
                argv.push(prefix.into());
            }
            if let Some(older_than) = text("older_than")? {
                argv.push("--older-than".into());
                argv.push(older_than.into());
            }
            if flag("delete_branches")? {
                argv.push("--delete-branches".into());
            }
            match arguments.get("allow_generated") {
                None | Some(Value::Null) => {}
                Some(Value::Array(paths)) => {
                    for path in paths {
                        let Value::String(path) = path else {
                            return Err(
                                "argument 'allow_generated' must be an array of strings".to_owned()
                            );
                        };
                        argv.push("--allow-generated".into());
                        argv.push(path.clone().into());
                    }
                }
                Some(_) => {
                    return Err("argument 'allow_generated' must be an array of strings".to_owned())
                }
            }
        }
        "prune" => {
            argv.push("prune".into());
            if flag("all")? {
                argv.push("--all".into());
            }
        }
        "prepare" => {
            argv.push("prepare".into());
            if flag("apply")? {
                argv.push("--apply".into());
            }
        }
        "migrate" => {
            argv.push("migrate".into());
            if flag("all")? {
                argv.push("--all".into());
            }
            if flag("apply")? {
                argv.push("--apply".into());
            }
            if let Some(baseline) = text("baseline")? {
                argv.push("--baseline".into());
                argv.push(baseline.into());
            }
            if flag("source_only")? {
                argv.push("--source-only".into());
            }
            if flag("adopt")? {
                argv.push("--adopt".into());
            }
        }
        other => return Err(format!("unknown tool: {other}")),
    }
    Ok((argv, repo))
}

/// Run the translated CLI invocation and shape its result for MCP. Failures
/// and refusals become `isError` tool results carrying the CLI's stderr, never
/// protocol errors, so the calling agent can read and surface the reason.
fn execute(argv: Vec<OsString>, repo: Option<PathBuf>) -> Value {
    let executable = match std::env::current_exe() {
        Ok(executable) => executable,
        Err(error) => return tool_error(&format!("locate wt0 executable: {error}")),
    };
    let mut command = Command::new(executable);
    command.args(&argv);
    if let Some(repo) = repo {
        command.current_dir(repo);
    }
    let output = match command.output() {
        Ok(output) => output,
        Err(error) => return tool_error(&format!("run wt0: {error}")),
    };
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !output.status.success() {
        let reason = if stderr.is_empty() { &stdout } else { &stderr };
        return tool_error(if reason.is_empty() {
            "wt0 failed without diagnostics"
        } else {
            reason
        });
    }
    match serde_json::from_str::<Value>(&stdout) {
        Ok(structured) => json!({
            "content": [{ "type": "text", "text": stdout }],
            "structuredContent": structured,
            "isError": false,
        }),
        // A payload that is not JSON (streamed or legacy output) is still a
        // successful result; it is returned as plain text.
        Err(_) => json!({
            "content": [{ "type": "text", "text": stdout }],
            "isError": false,
        }),
    }
}

fn tool_error(message: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": message }],
        "isError": true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initialize_negotiates_known_versions_and_offers_latest_otherwise() {
        let known = initialize_result(&json!({ "protocolVersion": "2025-06-18" }));
        assert_eq!(known["protocolVersion"], "2025-06-18");
        let unknown = initialize_result(&json!({ "protocolVersion": "1999-01-01" }));
        assert_eq!(unknown["protocolVersion"], SUPPORTED_PROTOCOL_VERSIONS[0]);
        assert_eq!(unknown["serverInfo"]["name"], "worktree-zero");
    }

    #[test]
    fn every_tool_translates_to_a_json_cli_invocation() {
        for tool in tools() {
            let mut arguments = Map::new();
            for required in tool.required {
                arguments.insert((*required).to_owned(), json!("value"));
            }
            let (argv, repo) = build_argv(tool.name, &arguments).expect(tool.name);
            assert_eq!(argv[0], OsString::from("--json"), "{}", tool.name);
            assert!(repo.is_none());
        }
    }

    #[test]
    fn arguments_are_validated_before_execution() {
        assert!(build_argv("create_worktree", &Map::new())
            .unwrap_err()
            .contains("'branch' is required"));
        let mut relative = Map::new();
        relative.insert("repo".to_owned(), json!("not/absolute"));
        assert!(build_argv("capabilities", &relative)
            .unwrap_err()
            .contains("absolute"));
        let mut wrong = Map::new();
        wrong.insert("apply".to_owned(), json!("yes"));
        assert!(build_argv("gc", &wrong).unwrap_err().contains("boolean"));
    }

    #[test]
    fn notifications_are_ignored_and_unknown_methods_are_errors() {
        assert!(
            handle_message(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none()
        );
        let response =
            handle_message(r#"{"jsonrpc":"2.0","id":7,"method":"resources/list"}"#).unwrap();
        assert_eq!(response["error"]["code"], -32601);
        let ping = handle_message(r#"{"jsonrpc":"2.0","id":8,"method":"ping"}"#).unwrap();
        assert_eq!(ping["result"], json!({}));
    }
}
