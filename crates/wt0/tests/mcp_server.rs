//! End-to-end test of `wt0 mcp serve`: a real MCP client conversation over
//! stdio — initialize, tools/list, and lifecycle tool calls — against a real
//! repository fixture.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl McpClient {
    fn start(repo: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_wt0"))
            .args(["mcp", "serve"])
            .current_dir(repo)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("start wt0 mcp serve");
        let stdin = child.stdin.take().expect("server stdin");
        let stdout = BufReader::new(child.stdout.take().expect("server stdout"));
        Self {
            child,
            stdin,
            stdout,
            next_id: 0,
        }
    }

    fn request(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.next_id += 1;
        let message = serde_json::json!({
            "jsonrpc": "2.0",
            "id": self.next_id,
            "method": method,
            "params": params,
        });
        writeln!(self.stdin, "{message}").expect("send request");
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read response");
        let response: serde_json::Value = serde_json::from_str(&line).expect("parse response");
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], self.next_id);
        response
    }

    fn notify(&mut self, method: &str) {
        let message = serde_json::json!({ "jsonrpc": "2.0", "method": method });
        writeln!(self.stdin, "{message}").expect("send notification");
    }

    fn call(&mut self, tool: &str, arguments: serde_json::Value) -> serde_json::Value {
        let response = self.request(
            "tools/call",
            serde_json::json!({ "name": tool, "arguments": arguments }),
        );
        assert!(
            response.get("error").is_none(),
            "tool {tool} returned a protocol error: {response}"
        );
        response["result"].clone()
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .current_dir(repo)
        .args(args)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?}");
}

#[test]
fn mcp_serve_speaks_the_lifecycle_end_to_end() {
    let root = std::env::temp_dir().join(format!(
        "wt0-mcp-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let repo = root.join("repo");
    fs::create_dir_all(&repo).expect("create repository");
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "test@example.com"]);
    git(&repo, &["config", "user.name", "Test User"]);
    fs::write(repo.join("README.md"), "base\n").expect("write fixture");
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "-q", "-m", "initial"]);

    let mut client = McpClient::start(&repo);

    let initialize = client.request(
        "initialize",
        serde_json::json!({
            "protocolVersion": "2026-07-28",
            "capabilities": {},
            "clientInfo": { "name": "wt0-test-client", "version": "1.0.0" },
        }),
    );
    assert_eq!(initialize["result"]["protocolVersion"], "2026-07-28");
    assert_eq!(initialize["result"]["serverInfo"]["name"], "worktree-zero");
    client.notify("notifications/initialized");

    let tools = client.request("tools/list", serde_json::json!({}));
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .expect("tool list")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect();
    for required in [
        "capabilities",
        "create_worktree",
        "list_worktrees",
        "remove_worktree",
        "heartbeat",
        "gc",
        "prune",
        "migrate",
    ] {
        assert!(names.contains(&required), "missing tool {required}");
    }

    let capabilities = client.call("capabilities", serde_json::json!({}));
    assert_eq!(capabilities["isError"], false);
    assert_eq!(
        capabilities["structuredContent"]["protocol"]["mcp"],
        "shipped"
    );

    let worktree_path = root.join("agent-worktree");
    let created = client.call(
        "create_worktree",
        serde_json::json!({
            "branch": "agent/mcp-test",
            "path": worktree_path.to_string_lossy(),
        }),
    );
    assert_eq!(created["isError"], false, "create failed: {created}");
    assert_eq!(
        created["structuredContent"]["worktree"],
        worktree_path.to_string_lossy().as_ref()
    );
    assert!(worktree_path.join("README.md").is_file());

    let heartbeat = client.call(
        "heartbeat",
        serde_json::json!({ "target": worktree_path.to_string_lossy() }),
    );
    assert_eq!(heartbeat["isError"], false, "heartbeat failed: {heartbeat}");

    // A refusal surfaces as an isError tool result with the reason, never as
    // a protocol error: creating the same branch twice must be refused.
    let duplicate = client.call(
        "create_worktree",
        serde_json::json!({ "branch": "agent/mcp-test" }),
    );
    assert_eq!(duplicate["isError"], true);
    assert!(duplicate["content"][0]["text"]
        .as_str()
        .expect("refusal text")
        .contains("already exists"));

    let removed = client.call(
        "remove_worktree",
        serde_json::json!({
            "target": worktree_path.to_string_lossy(),
            "delete_branch": true,
        }),
    );
    assert_eq!(removed["isError"], false, "remove failed: {removed}");
    assert!(!worktree_path.exists());

    let unknown = client.request(
        "tools/call",
        serde_json::json!({ "name": "does_not_exist", "arguments": {} }),
    );
    assert_eq!(unknown["error"]["code"], -32602);

    drop(client);
    let _ = fs::remove_dir_all(root);
}
