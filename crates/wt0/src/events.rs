//! Append-only lifecycle event log.
//!
//! Every lifecycle transition — created, reused, removed, reaped, adopted —
//! appends one JSON line to `.git/wt0/events.jsonl`, so orchestrators observe
//! a fleet without polling dry-run receipts. Recording is best-effort by
//! design: an unwritable log warns on stderr but never fails the operation
//! it describes, and the log is an observability surface, never an ownership
//! record — receipts and markers stay authoritative.

use anyhow::{Context, Result};
use clap::Args;
use serde_json::{json, Value};
use std::fs;
use std::io::{BufRead, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const EVENTS_FILE: &str = "events.jsonl";

/// Best-effort append of one event line. `fields` supplies event-specific
/// keys; `ts_unix` and `event` are added here.
pub(crate) fn record(common_git_dir: &Path, event: &str, mut fields: Value) {
    let result = (|| -> Result<()> {
        let dir = crate::commands::worktree::state_dir(common_git_dir);
        fs::create_dir_all(&dir)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0);
        if let Some(map) = fields.as_object_mut() {
            map.insert("ts_unix".to_owned(), json!(now));
            map.insert("event".to_owned(), json!(event));
        }
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(EVENTS_FILE))?;
        let mut line = serde_json::to_vec(&fields)?;
        line.push(b'\n');
        file.write_all(&line)?;
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!("wt0: could not record {event} event: {error:#}");
    }
}

#[derive(Args, Default)]
pub struct Events {
    /// Print at most this many of the newest events.
    #[arg(long, default_value_t = 50)]
    pub limit: usize,

    /// Keep the log open and stream new events as they are appended.
    #[arg(long, conflicts_with = "json")]
    pub follow: bool,

    /// Wrap the events in one versioned JSON object instead of JSONL.
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: Events, global_json: bool) -> Result<()> {
    let repo = crate::commands::worktree::discover_repo(&std::env::current_dir()?)?;
    let path = crate::commands::worktree::state_dir(&repo.common_git_dir).join(EVENTS_FILE);
    let lines = tail_lines(&path, args.limit)?;
    if args.json || global_json {
        let events: Vec<Value> = lines
            .iter()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "schema_version": 1,
                "events": events,
            }))?
        );
        return Ok(());
    }
    for line in &lines {
        println!("{line}");
    }
    if args.follow {
        follow(&path)?;
    }
    Ok(())
}

fn tail_lines(path: &PathBuf, limit: usize) -> Result<Vec<String>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).context("read lifecycle event log"),
    };
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let start = lines.len().saturating_sub(limit);
    Ok(lines[start..]
        .iter()
        .map(|line| (*line).to_owned())
        .collect())
}

/// Poll-based tail: stream lines appended after the current end of the log.
fn follow(path: &Path) -> Result<()> {
    let mut offset = fs::metadata(path).map(|meta| meta.len()).unwrap_or(0);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let length = match fs::metadata(path) {
            Ok(meta) => meta.len(),
            Err(_) => continue,
        };
        if length < offset {
            // The log was truncated or rotated; restart from the beginning.
            offset = 0;
        }
        if length == offset {
            continue;
        }
        let mut file = fs::File::open(path).context("open lifecycle event log")?;
        file.seek(SeekFrom::Start(offset))
            .context("seek lifecycle event log")?;
        let reader = std::io::BufReader::new(&mut file);
        for line in reader.lines() {
            let line = line.context("read appended event")?;
            if !line.trim().is_empty() {
                println!("{line}");
            }
        }
        offset = length;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_append_as_json_lines_and_tail_respects_the_limit() {
        let root = std::env::temp_dir().join(format!("wt0-events-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).expect("create fixture");
        record(&root, "created", json!({ "branch": "agent/a" }));
        record(&root, "removed", json!({ "branch": "agent/a" }));
        record(&root, "created", json!({ "branch": "agent/b" }));

        let path = crate::commands::worktree::state_dir(&root).join(EVENTS_FILE);
        let all = tail_lines(&path, 50).expect("tail");
        assert_eq!(all.len(), 3);
        let first: Value = serde_json::from_str(&all[0]).expect("event json");
        assert_eq!(first["event"], "created");
        assert_eq!(first["branch"], "agent/a");
        assert!(first["ts_unix"].as_u64().is_some());

        let last_two = tail_lines(&path, 2).expect("tail limit");
        assert_eq!(last_two.len(), 2);
        let last: Value = serde_json::from_str(&last_two[1]).expect("event json");
        assert_eq!(last["branch"], "agent/b");
        let _ = fs::remove_dir_all(&root);
    }
}
