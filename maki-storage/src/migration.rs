//! Legacy flat `<id>.jsonl` → tree-folder migration (§6).
//!
//! On load: detect legacy flat file (no `header` node or `version < 2`),
//! migrate in-memory. Keep legacy file untouched; next save rewrites into the
//! new folder layout and removes the old file.
//!
//! Legacy `LogRecord` variants map to tree nodes:
//! - `Header` → `Header` (new field set, `version = 2`)
//! - `Msg { d }` → `Message` node (payloads extracted per §5)
//! - `Out { id, d }` → `payloads.jsonl` records keyed by `tool_use_id`
//! - `SubMsg` → kept as side-channel (no tree node through PR3, §17)
//! - `Meta` → `meta.json` (title, token_usage, updated_at, SessionMeta)
//!
//! Legacy `do_compact` was destructive; pre-compaction messages are
//! unrecoverable. Migration appends a terminal `Compaction` node with the
//! legacy summary and `first_kept_id` = last message id on disk (§6).

use std::fs;
use std::path::{Path, PathBuf};

use maki_util::EntityId;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::warn;

use crate::session_log::{PAYLOADS_FILE, SessionFolder, TREE_FILE};
use crate::sessions::{SessionError, SessionMeta};
use crate::tree::{
    Compaction, Header, LeafEntry, Lineage, MessageNode, Node, NodeId, PayloadRecord,
    SessionMetaFile, TREE_FORMAT_VERSION, new_compaction_id, new_leaf_id, new_message_id,
    new_payload_id,
};

#[cfg_attr(not(test), allow(dead_code))]
const LEGACY_LOG_FORMAT_VERSION: u32 = 2;
const TIMESTAMP_LEGACY_DEFAULT: u64 = 0;
/// Result of migrating a legacy session in-memory.
pub struct MigratedSession {
    pub header: Header,
    pub nodes: Vec<Node>,
    pub payloads: Vec<PayloadRecord>,
    pub meta: SessionMetaFile,
    pub lineage: Lineage,
    pub legacy_path: PathBuf,
    /// Terminal compaction node id, if the legacy session was compacted.
    pub legacy_compaction_node_id: Option<NodeId>,
}

/// Detects and loads a legacy flat `<id>.jsonl` or `<id>.json` session,
/// migrating it to tree-node shape in memory.
///
/// Returns `None` if the path is neither a legacy JSONL nor legacy JSON file
/// (e.g., already a tree-folder session).
pub fn migrate_legacy<M, U, T>(
    session_id: &str,
    sessions_dir: &Path,
) -> Result<Option<MigratedSession>, SessionError>
where
    M: Serialize + DeserializeOwned,
    U: Serialize + DeserializeOwned + Default,
    T: Serialize + DeserializeOwned + Default,
{
    let jsonl_path = sessions_dir.join(format!("{session_id}.jsonl"));
    if jsonl_path.exists() {
        return migrate_jsonl::<M, T>(&jsonl_path).map(Some);
    }

    let json_path = sessions_dir.join(format!("{session_id}.json"));
    if json_path.exists() {
        return migrate_legacy_json::<M, U, T>(&json_path).map(Some);
    }

    Ok(None)
}

fn parse_legacy_id(raw: &str) -> Result<EntityId, SessionError> {
    raw.parse::<EntityId>()
        .map_err(|e| SessionError::CorruptTree(format!("legacy session id {raw:?}: {e}")))
}

#[derive(Deserialize)]
#[serde(tag = "t")]
enum LegacyRecord<M, T> {
    #[serde(rename = "header")]
    Header {
        v: u32,
        id: String,
        model: String,
        cwd: String,
        created_at: u64,
    },
    #[serde(rename = "msg")]
    Msg { d: M },
    #[serde(rename = "out")]
    Out { id: String, d: T },
    #[serde(rename = "sub_msg")]
    #[allow(dead_code)]
    SubMsg { sub: String, d: M },
    #[serde(rename = "meta")]
    Meta {
        title: String,
        token_usage: Value,
        updated_at: u64,
        #[serde(flatten)]
        meta: SessionMeta,
    },
}

fn migrate_jsonl<M, T>(path: &Path) -> Result<MigratedSession, SessionError>
where
    M: Serialize + DeserializeOwned,
    T: Serialize + DeserializeOwned,
{
    let content = fs::read_to_string(path).map_err(crate::StorageError::from)?;
    let lines = content.lines();

    let mut header: Option<Header> = None;
    let mut messages: Vec<M> = Vec::new();
    let mut tool_outputs: std::collections::HashMap<String, T> = std::collections::HashMap::new();
    let mut title = String::new();
    let mut token_usage = Value::Null;
    let mut updated_at = TIMESTAMP_LEGACY_DEFAULT;
    let mut meta = SessionMeta::default();
    let mut created_at = TIMESTAMP_LEGACY_DEFAULT;
    let mut model = String::new();

    for (idx, line) in lines.enumerate() {
        if line.is_empty() {
            continue;
        }
        let record: LegacyRecord<M, T> = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                warn!(path = %path.display(), error = %e, line = idx, "skipping unparseable legacy record");
                continue;
            }
        };
        match record {
            LegacyRecord::Header {
                v,
                id,
                model: m,
                cwd,
                created_at: ca,
            } => {
                model = m;
                created_at = ca;
                header = Some(Header {
                    version: TREE_FORMAT_VERSION,
                    session_id: parse_legacy_id(&id)?,
                    cwd,
                    created_at: ca,
                    parent_session_id: None,
                });
                let _ = v;
            }
            LegacyRecord::Msg { d } => messages.push(d),
            LegacyRecord::Out { id, d } => {
                tool_outputs.insert(id, d);
            }
            LegacyRecord::SubMsg { .. } => {}
            LegacyRecord::Meta {
                title: t,
                token_usage: tu,
                updated_at: ua,
                meta: m,
            } => {
                title = t;
                token_usage = tu;
                updated_at = ua;
                meta = m;
            }
        }
    }

    let header =
        header.ok_or_else(|| SessionError::CorruptTree("legacy JSONL missing header".into()))?;

    build_migrated(
        path,
        header,
        messages,
        tool_outputs,
        title,
        token_usage,
        updated_at,
        meta,
        created_at,
        model,
    )
}

fn migrate_legacy_json<M, U, T>(path: &Path) -> Result<MigratedSession, SessionError>
where
    M: Serialize + DeserializeOwned,
    U: Serialize + DeserializeOwned + Default,
    T: Serialize + DeserializeOwned + Default,
{
    #[derive(Deserialize)]
    #[allow(dead_code)]
    struct LegacyJsonSession<M, U, T> {
        version: u32,
        id: String,
        title: String,
        cwd: String,
        model: String,
        messages: Vec<M>,
        #[serde(default)]
        tool_outputs: std::collections::HashMap<String, T>,
        #[serde(flatten)]
        meta: SessionMeta,
        created_at: u64,
        updated_at: u64,
        #[serde(default)]
        token_usage: U,
    }

    let data = fs::read(path).map_err(crate::StorageError::from)?;
    let session: LegacyJsonSession<M, U, T> =
        serde_json::from_slice(&data).map_err(crate::StorageError::from)?;

    let header = Header {
        version: TREE_FORMAT_VERSION,
        session_id: parse_legacy_id(&session.id)?,
        cwd: session.cwd,
        created_at: session.created_at,
        parent_session_id: None,
    };

    build_migrated(
        path,
        header,
        session.messages,
        session.tool_outputs,
        session.title,
        serde_json::to_value(&session.token_usage).unwrap_or(Value::Null),
        session.updated_at,
        session.meta,
        session.created_at,
        session.model,
    )
}

#[allow(clippy::too_many_arguments)]
fn build_migrated<M, T>(
    legacy_path: &Path,
    header: Header,
    messages: Vec<M>,
    tool_outputs: std::collections::HashMap<String, T>,
    title: String,
    token_usage: Value,
    updated_at: u64,
    meta: SessionMeta,
    created_at: u64,
    model: String,
) -> Result<MigratedSession, SessionError>
where
    M: Serialize,
    T: Serialize,
{
    let session_id = header.session_id.to_string();
    let mut nodes = vec![Node::Header(header.clone())];
    let mut payloads = Vec::new();
    let mut legacy_compaction_node_id = None;

    let mut parent_id = session_id.clone();

    let tool_outputs_json: std::collections::HashMap<String, Value> = tool_outputs
        .into_iter()
        .map(|(k, v)| (k, serde_json::to_value(&v).unwrap_or(Value::Null)))
        .collect();

    let is_likely_compacted = is_likely_compacted_messages(&messages);

    for msg in &messages {
        let msg_value = serde_json::to_value(msg).unwrap_or(Value::Null);
        let node_id = new_message_id();
        let (content_blocks, extracted_payloads) =
            extract_payloads(&msg_value, &tool_outputs_json, &node_id);

        for payload in extracted_payloads {
            payloads.push(payload);
        }

        let node = MessageNode {
            id: node_id.clone(),
            parent_id: parent_id.clone(),
            role: msg_value
                .get("role")
                .and_then(|r| r.as_str())
                .unwrap_or("user")
                .to_string(),
            content_blocks,
            timestamp: created_at,
            run_id: None,
        };
        parent_id = node_id;
        nodes.push(Node::Message(node));
    }

    if is_likely_compacted {
        let summary = extract_compaction_summary(&messages);
        let cmp_id = new_compaction_id();
        legacy_compaction_node_id = Some(cmp_id.clone());
        nodes.push(Node::Compaction(Compaction {
            id: cmp_id,
            parent_id: parent_id.clone(),
            summary,
            first_kept_id: parent_id.clone(),
            timestamp: created_at,
        }));
    }

    if let Some(last_msg_id) = messages_last_node_id(&nodes) {
        let leaf_id = new_leaf_id();
        nodes.push(Node::Leaf(LeafEntry {
            id: leaf_id,
            parent_id: session_id.clone(),
            target_node_id: last_msg_id,
        }));
    }

    let meta_file = SessionMetaFile {
        title,
        token_usage,
        updated_at,
        created_at,
        model: Some(model),
        meta,
    };

    Ok(MigratedSession {
        header,
        nodes,
        payloads,
        meta: meta_file,
        lineage: Lineage::default(),
        legacy_path: legacy_path.to_path_buf(),
        legacy_compaction_node_id,
    })
}

fn messages_last_node_id(nodes: &[Node]) -> Option<String> {
    nodes
        .iter()
        .rev()
        .find(|n| matches!(n, Node::Message(_) | Node::Compaction(_)))
        .map(|n| n.id().to_string())
}

fn extract_payloads(
    msg_value: &Value,
    tool_outputs: &std::collections::HashMap<String, Value>,
    session_id: &str,
) -> (Vec<Value>, Vec<PayloadRecord>) {
    let Some(blocks) = msg_value.get("content").and_then(|c| c.as_array()) else {
        return (Vec::new(), Vec::new());
    };

    let mut out_blocks = Vec::with_capacity(blocks.len());
    let mut payloads = Vec::new();

    for (idx, block) in blocks.iter().enumerate() {
        let block_type = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match block_type {
            "tool_result" => {
                let tool_use_id = block
                    .get("tool_use_id")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                let payload_id = new_payload_id();
                let content = tool_outputs
                    .get(&tool_use_id)
                    .and_then(|v| v.as_str())
                    .or_else(|| block.get("content").and_then(|c| c.as_str()))
                    .unwrap_or("")
                    .to_string();
                let is_error = block
                    .get("is_error")
                    .and_then(|e| e.as_bool())
                    .unwrap_or(false);
                payloads.push(PayloadRecord::ToolResult {
                    id: payload_id.clone(),
                    node_id: session_id.to_string(),
                    block_idx: idx,
                    content,
                    is_error,
                });
                out_blocks.push(serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": tool_use_id,
                    "content": {"kind": "ref", "payloadId": payload_id},
                    "is_error": is_error,
                }));
            }
            "image" => {
                let payload_id = new_payload_id();
                if let Some(source) = block.get("source") {
                    let media_type = source
                        .get("media_type")
                        .and_then(|m| m.as_str())
                        .unwrap_or("png")
                        .to_string();
                    let data = source
                        .get("data")
                        .and_then(|d| d.as_str())
                        .unwrap_or("")
                        .to_string();
                    payloads.push(PayloadRecord::Image {
                        id: payload_id.clone(),
                        node_id: session_id.to_string(),
                        block_idx: idx,
                        media_type,
                        data,
                    });
                    out_blocks.push(serde_json::json!({
                        "type": "image",
                        "source": {"kind": "ref", "payloadId": payload_id},
                    }));
                } else {
                    out_blocks.push(block.clone());
                }
            }
            _ => out_blocks.push(block.clone()),
        }
    }

    (out_blocks, payloads)
}

const LEGACY_COMPACTION_MARKER: &str = "What did we do so far?";

/// Heuristic: `finish_compact` (`maki-agent/src/agent/compaction.rs`) replaces
/// history with `[Message::user("What did we do so far?"), assistant_summary]`.
/// Detect that exact shape so the terminal `Compaction` node is appended and
/// post-compaction tail is honest (§6).
fn is_likely_compacted_messages<M: Serialize>(messages: &[M]) -> bool {
    if messages.len() < 2 {
        return false;
    }
    let first = serde_json::to_value(&messages[0]).unwrap_or(Value::Null);
    let second = serde_json::to_value(&messages[1]).unwrap_or(Value::Null);

    let first_is_marker = first.get("role").and_then(|r| r.as_str()) == Some("user")
        && first_text(&first).is_some_and(|t| t == LEGACY_COMPACTION_MARKER);
    let second_is_assistant = second.get("role").and_then(|r| r.as_str()) == Some("assistant");

    first_is_marker && second_is_assistant
}

fn first_text(value: &Value) -> Option<&str> {
    value
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first())
        .and_then(|b| b.get("text"))
        .and_then(|t| t.as_str())
}

fn extract_compaction_summary<M: Serialize>(messages: &[M]) -> String {
    if messages.len() < 2 {
        return String::new();
    }
    let second = serde_json::to_value(&messages[1]).unwrap_or(Value::Null);
    second
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first())
        .and_then(|b| b.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string()
}

/// Writes a migrated session to the new folder layout and removes the legacy file.
pub fn write_migrated(
    migrated: MigratedSession,
    sessions_dir: &Path,
) -> Result<SessionFolder, SessionError> {
    let session_dir = sessions_dir.join(migrated.header.session_id.to_string());
    fs::create_dir_all(&session_dir).map_err(crate::StorageError::from)?;
    crate::session_log::sync_parent_dir_of(&session_dir);

    let tree_path = session_dir.join(TREE_FILE);
    let mut buf = Vec::new();
    for node in &migrated.nodes {
        serde_json::to_writer(&mut buf, node).map_err(crate::StorageError::from)?;
        buf.push(b'\n');
    }
    crate::atomic_write(&tree_path, &buf)?;

    let payloads_path = session_dir.join(PAYLOADS_FILE);
    if !migrated.payloads.is_empty() {
        let mut buf = Vec::new();
        for payload in &migrated.payloads {
            serde_json::to_writer(&mut buf, payload).map_err(crate::StorageError::from)?;
            buf.push(b'\n');
        }
        crate::atomic_write(&payloads_path, &buf)?;
    } else {
        use std::fs::OpenOptions;
        use std::io::Write;
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&payloads_path)
            .map_err(crate::StorageError::from)?;
        f.write_all(b"").map_err(crate::StorageError::from)?;
        f.sync_data().map_err(crate::StorageError::from)?;
        crate::session_log::sync_parent_dir_of(&payloads_path);
    }

    let meta_path = session_dir.join("meta.json");
    let meta_data = serde_json::to_vec_pretty(&migrated.meta).map_err(crate::StorageError::from)?;
    crate::atomic_write(&meta_path, &meta_data)?;

    let lineage_path = session_dir.join("lineage.json");
    let lineage_data =
        serde_json::to_vec_pretty(&migrated.lineage).map_err(crate::StorageError::from)?;
    crate::atomic_write(&lineage_path, &lineage_data)?;

    let _ = fs::remove_file(&migrated.legacy_path);

    SessionFolder::open(&session_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{Node, PayloadRecord};
    use serde_json::json;
    use tempfile::TempDir;

    const LEGACY_SESSION_ID_HEX: &str = "01965087-4c71-7f00-8000-000000000000";
    const LEGACY_CWD: &str = "/test";
    const LEGACY_CREATED_AT: u64 = 5000;

    fn legacy_session_id() -> EntityId {
        LEGACY_SESSION_ID_HEX.parse().unwrap()
    }

    fn write_legacy_jsonl(dir: &Path, id: &str, lines: Vec<String>) -> PathBuf {
        let path = dir.join(format!("{id}.jsonl"));
        let mut content = lines.join("\n");
        if !content.is_empty() {
            content.push('\n');
        }
        fs::write(&path, content).unwrap();
        path
    }

    fn legacy_header_line(id: &str) -> String {
        json!({
            "t": "header",
            "v": LEGACY_LOG_FORMAT_VERSION,
            "id": id,
            "model": "claude",
            "cwd": LEGACY_CWD,
            "created_at": LEGACY_CREATED_AT
        })
        .to_string()
    }

    fn legacy_msg_line(role: &str, text: &str) -> String {
        json!({
            "t": "msg",
            "d": {"role": role, "content": [{"type": "text", "text": text}]}
        })
        .to_string()
    }

    fn legacy_out_line(id: &str, content: &str) -> String {
        json!({"t": "out", "id": id, "d": {"result": content}}).to_string()
    }

    fn legacy_meta_line(title: &str) -> String {
        json!({
            "t": "meta",
            "title": title,
            "token_usage": {"input": 100},
            "updated_at": LEGACY_CREATED_AT,
            "mode": "build"
        })
        .to_string()
    }

    #[test]
    fn migrate_jsonl_linear_chain() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        write_legacy_jsonl(
            dir,
            LEGACY_SESSION_ID_HEX,
            vec![
                legacy_header_line(LEGACY_SESSION_ID_HEX),
                legacy_msg_line("user", "Hello"),
                legacy_msg_line("assistant", "Hi there"),
                legacy_meta_line("Test Title"),
            ],
        );

        let migrated = migrate_legacy::<Value, Value, Value>(LEGACY_SESSION_ID_HEX, dir)
            .unwrap()
            .expect("should find legacy session");

        assert_eq!(migrated.header.session_id, legacy_session_id());
        assert_eq!(migrated.header.version, TREE_FORMAT_VERSION);
        assert_eq!(migrated.header.cwd, LEGACY_CWD);

        let session_id_str = migrated.header.session_id.to_string();
        let msg_nodes: Vec<&Node> = migrated
            .nodes
            .iter()
            .filter(|n| matches!(n, Node::Message(_)))
            .collect();
        assert_eq!(msg_nodes.len(), 2);
        assert_eq!(msg_nodes[0].parent_id(), Some(session_id_str.as_str()));
        let first_msg_id = msg_nodes[0].id().to_string();
        assert_eq!(msg_nodes[1].parent_id(), Some(first_msg_id.as_str()));

        assert!(migrated.nodes.iter().any(|n| matches!(n, Node::Leaf(_))));
        assert_eq!(migrated.meta.title, "Test Title");
        assert!(migrated.legacy_compaction_node_id.is_none());
    }

    #[test]
    fn migrate_extracts_tool_result_payloads() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        write_legacy_jsonl(
            dir,
            LEGACY_SESSION_ID_HEX,
            vec![
                legacy_header_line(LEGACY_SESSION_ID_HEX),
                json!({
                    "t": "msg",
                    "d": {
                        "role": "assistant",
                        "content": [{"type": "tool_use", "id": "tu_1", "name": "bash", "input": {}}]
                    }
                })
                .to_string(),
                json!({
                    "t": "msg",
                    "d": {
                        "role": "user",
                        "content": [{"type": "tool_result", "tool_use_id": "tu_1", "content": "result data"}]
                    }
                })
                .to_string(),
                legacy_out_line("tu_1", "result data"),
                legacy_meta_line("T"),
            ],
        );

        let migrated = migrate_legacy::<Value, Value, Value>(LEGACY_SESSION_ID_HEX, dir)
            .unwrap()
            .expect("should find");

        assert_eq!(migrated.payloads.len(), 1);
        match &migrated.payloads[0] {
            PayloadRecord::ToolResult { content, .. } => {
                assert!(content.contains("result data") || content == "result data");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }

        let msg_with_ref = migrated
            .nodes
            .iter()
            .find_map(|n| match n {
                Node::Message(m) if m.role == "user" => Some(m),
                _ => None,
            })
            .expect("should have user message");
        let block = &msg_with_ref.content_blocks[0];
        assert_eq!(
            block.get("type").and_then(|t| t.as_str()),
            Some("tool_result")
        );
        assert_eq!(
            block
                .get("content")
                .and_then(|c| c.get("kind"))
                .and_then(|k| k.as_str()),
            Some("ref")
        );
    }

    #[test]
    fn migrate_compacted_session_adds_terminal_compaction() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        write_legacy_jsonl(
            dir,
            LEGACY_SESSION_ID_HEX,
            vec![
                legacy_header_line(LEGACY_SESSION_ID_HEX),
                json!({
                    "t": "msg",
                    "d": {
                        "role": "user",
                        "content": [{"type": "text", "text": "What did we do so far?"}]
                    }
                })
                .to_string(),
                json!({
                    "t": "msg",
                    "d": {
                        "role": "assistant",
                        "content": [{"type": "text", "text": "summary of prior context"}]
                    }
                })
                .to_string(),
                legacy_meta_line("Compacted"),
            ],
        );

        let migrated = migrate_legacy::<Value, Value, Value>(LEGACY_SESSION_ID_HEX, dir)
            .unwrap()
            .expect("should find");

        assert!(migrated.legacy_compaction_node_id.is_some());
        let compaction = migrated
            .nodes
            .iter()
            .find_map(|n| match n {
                Node::Compaction(c) => Some(c),
                _ => None,
            })
            .expect("should have terminal compaction");
        assert!(compaction.summary.contains("summary of prior context"));
    }

    #[test]
    fn migrate_returns_none_when_no_legacy_file() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        let result = migrate_legacy::<Value, Value, Value>("nonexistent", dir).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn write_migrated_creates_folder_and_removes_legacy() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        let legacy_path = write_legacy_jsonl(
            dir,
            LEGACY_SESSION_ID_HEX,
            vec![
                legacy_header_line(LEGACY_SESSION_ID_HEX),
                legacy_msg_line("user", "Hello"),
                legacy_meta_line("T"),
            ],
        );
        assert!(legacy_path.exists());

        let migrated = migrate_legacy::<Value, Value, Value>(LEGACY_SESSION_ID_HEX, dir)
            .unwrap()
            .expect("should find");

        let folder = write_migrated(migrated, dir).unwrap();

        assert!(!legacy_path.exists());
        assert!(folder.dir().join(TREE_FILE).exists());
        assert!(folder.dir().join(PAYLOADS_FILE).exists());
        assert_eq!(folder.nodes.len(), 3);
    }

    #[test]
    fn migrate_jsonl_preserves_submsg_as_noop() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();

        write_legacy_jsonl(
            dir,
            LEGACY_SESSION_ID_HEX,
            vec![
                legacy_header_line(LEGACY_SESSION_ID_HEX),
                json!({"t": "sub_msg", "sub": "sub_1", "d": {"role": "assistant", "content": []}})
                    .to_string(),
                legacy_msg_line("user", "hi"),
                legacy_meta_line("T"),
            ],
        );

        let migrated = migrate_legacy::<Value, Value, Value>(LEGACY_SESSION_ID_HEX, dir)
            .unwrap()
            .expect("should find");

        let msg_nodes = migrated
            .nodes
            .iter()
            .filter(|n| matches!(n, Node::Message(_)))
            .count();
        assert_eq!(msg_nodes, 1);
    }
}
