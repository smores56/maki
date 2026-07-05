//! Corpus migration harness: migrates every legacy `.jsonl`/`.json` session
//! in `~/.local/state/maki/sessions/` to the tree-folder layout and verifies
//! each migrated session reopens cleanly via `SessionFolder`.
//!
//! Run: `cargo run --example migrate_corpus -- <sessions_dir>`
//! Defaults to `~/.local/state/maki/sessions/`.
//!
//! Originals are untouched: each legacy file is copied into a tempdir and
//! migration runs on the copy. A separate output dir receives the migrated
//! folders so re-runs are deterministic.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use maki_providers::{Message, TokenUsage};
use maki_storage::migration::{migrate_legacy, write_migrated};
use maki_storage::session_log::SessionFolder;
use maki_storage::tree::Node;
use serde_json::Value;

#[derive(Default)]
struct Stats {
    total: usize,
    success: usize,
    skipped: usize,
    failed: Vec<(PathBuf, String)>,
}

fn main() -> ExitCode {
    let sessions_dir = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(default_sessions_dir);

    if !sessions_dir.exists() {
        eprintln!("sessions dir not found: {}", sessions_dir.display());
        return ExitCode::from(2);
    }

    let entries = match collect_legacy(&sessions_dir) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("error reading {}: {e}", sessions_dir.display());
            return ExitCode::from(2);
        }
    };

    let work_dir = std::env::temp_dir().join("maki-corpus-migration");
    let _ = std::fs::remove_dir_all(&work_dir);
    std::fs::create_dir_all(&work_dir).expect("create work dir");

    let out_dir = std::env::temp_dir().join("maki-corpus-migrated");
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).expect("create out dir");

    let stats = run_corpus(&entries, &work_dir, &out_dir);
    print_report(&sessions_dir, &stats);

    if stats.failed.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

fn default_sessions_dir() -> PathBuf {
    let home = std::env::var("HOME").expect("HOME");
    PathBuf::from(home)
        .join(".local")
        .join("state")
        .join("maki")
        .join("sessions")
}

const CWD_INDEX_FILE: &str = "cwd_latest.json";

fn collect_legacy(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            if p.file_name().and_then(|s| s.to_str()) == Some(CWD_INDEX_FILE) {
                return false;
            }
            matches!(
                p.extension().and_then(|x| x.to_str()),
                Some("jsonl") | Some("json")
            ) && p.metadata().map(|m| m.len() > 0).unwrap_or(false)
        })
        .collect();
    v.sort();
    Ok(v)
}

fn run_corpus(entries: &[PathBuf], work_dir: &Path, out_dir: &Path) -> Stats {
    let mut stats = Stats {
        total: entries.len(),
        ..Default::default()
    };

    for src in entries {
        let session_id = src
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let in_dir = work_dir.join(&session_id);
        std::fs::create_dir_all(&in_dir).expect("create in_dir");
        let copy = in_dir.join(src.file_name().unwrap());
        if let Err(e) = std::fs::copy(src, &copy) {
            stats
                .failed
                .push((src.clone(), format!("copy failed: {e}")));
            continue;
        }

        match migrate_legacy::<Message, TokenUsage, Value>(&session_id, &in_dir) {
            Ok(Some(migrated)) => {
                let folder = match write_migrated(migrated, out_dir) {
                    Ok(folder) => folder,
                    Err(e) => {
                        stats
                            .failed
                            .push((src.clone(), format!("write_migrated: {e}")));
                        continue;
                    }
                };
                if let Err(e) = verify_reopen(&folder, src) {
                    stats.failed.push((src.clone(), e));
                    continue;
                }
                stats.success += 1;
            }
            Ok(None) => stats.skipped += 1,
            Err(e) => {
                stats
                    .failed
                    .push((src.clone(), format!("migrate_legacy: {e}")));
            }
        }
    }

    stats
}

fn verify_reopen(folder: &SessionFolder, _src: &Path) -> Result<(), String> {
    let dir = folder.dir();
    let reopened = SessionFolder::open(dir).map_err(|e| format!("reopen: {e}"))?;

    if reopened.nodes.is_empty() {
        return Err("no nodes after reopen".into());
    }
    let header_count = reopened
        .nodes
        .iter()
        .filter(|n| matches!(n, Node::Header(_)))
        .count();
    if header_count != 1 {
        return Err(format!("expected 1 header, found {header_count}"));
    }

    if let Some(last_msg) = reopened
        .nodes
        .iter()
        .rev()
        .find(|n| matches!(n, Node::Message(_)))
    {
        let path = reopened.walk_to_root(last_msg.id());
        if path.is_empty() {
            return Err("walk_to_root from latest message is empty".into());
        }
        let mut ids: Vec<&str> = path.iter().map(|n| n.id()).collect();
        let len = ids.len();
        ids.sort();
        ids.dedup();
        if ids.len() != len {
            return Err("walk_to_root contains duplicate nodes (cycle)".into());
        }
    }

    for node in &reopened.nodes {
        let Node::Message(m) = node else {
            continue;
        };
        for (idx, block) in m.content_blocks.iter().enumerate() {
            if block.get("kind").and_then(|k| k.as_str()) != Some("ref") {
                continue;
            }
            let pid = block
                .get("payloadId")
                .and_then(|p| p.as_str())
                .unwrap_or("");
            if reopened.payload_by_id(pid).is_none()
                && reopened.payload_for(m.id.as_str(), idx).is_none()
            {
                return Err(format!("unresolved payload ref {pid} on message {}", m.id));
            }
        }
    }

    Ok(())
}

fn print_report(src_dir: &Path, stats: &Stats) {
    println!("corpus: {}", src_dir.display());
    println!("  total:   {}", stats.total);
    println!("  success: {}", stats.success);
    println!("  skipped: {}", stats.skipped);
    println!("  failed:  {}", stats.failed.len());
    for (p, e) in &stats.failed {
        println!(
            "    - {}: {e}",
            p.file_name().unwrap_or_default().to_string_lossy()
        );
    }
}
