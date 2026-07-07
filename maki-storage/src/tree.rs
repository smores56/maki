//! Tree record types for `tree.jsonl`.
//!
//! PR-A ships the v2 on-disk shape with linear history only (no branching yet):
//! each `MessageNode` chains to the previous via `parent_id`, and a terminal
//! `LeafRecord` marks the active tip. The record taxonomy matches the design
//! (§3) so PR-B can add branching, summaries, and labels without a format bump.
//!
//! Serde-tagged by `t`, matching the legacy `LogRecord` convention. Line 1 is
//! always `Header`. Records are append-only; `meta.json` holds mutable chrome.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::payloads::{PayloadReader, PayloadWriter, StoredInTree};
use crate::sessions::SessionError;
use crate::StorageError;

pub const TREE_JSONL: &str = "tree.jsonl";
pub const META_FILE: &str = "meta.json";

const HEADER_TAG: &str = "header";
const MESSAGE_TAG: &str = "message";
const LEAF_TAG: &str = "leaf";
const SUMMARY_TAG: &str = "summary";
const LABEL_TAG: &str = "label";

/// Branded node id with a type prefix. UUIDv7 base58-encoded tail makes ids
/// lexicographically sortable and collision-free without a registry (§0).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(String);

impl NodeId {
    pub fn new(prefix: &str) -> Self {
        let id = uuid::Uuid::now_v7().to_string();
        Self(format!("{}{}", prefix, bs58::encode(id).into_string()))
    }

    pub fn for_message() -> Self {
        Self::new("msg_")
    }

    pub fn for_leaf() -> Self {
        Self::new("lft_")
    }

    pub fn from_raw(raw: String) -> Self {
        Self(raw)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Header {
    pub version: u32,
    pub session_id: String,
    pub cwd: String,
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_from_node_id: Option<NodeId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageNode {
    pub id: NodeId,
    pub parent_id: Option<NodeId>,
    pub role: String,
    pub content: Value,
    pub timestamp: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<u64>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub interrupted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeafRecord {
    pub id: NodeId,
    pub parent_id: Option<NodeId>,
    pub target_node_id: NodeId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummaryKind {
    Compaction,
    Branch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryRecord {
    pub id: NodeId,
    pub parent_id: NodeId,
    pub kind: SummaryKind,
    pub narrative: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fold_to_id: Option<NodeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fold_from_id: Option<NodeId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modified_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LabelRecord {
    pub id: NodeId,
    pub parent_id: NodeId,
    pub name: String,
}

/// Serde-tagged envelope for one line of `tree.jsonl`. The `content` field of a
/// `Message` is the inline `StoredInTree::encode` output (large blobs spilled to
/// `payloads.bin` and referenced by `payload_id`); the legacy opaque `M` no
/// longer appears here because externalization is the #453 compression win.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum TreeRecord {
    Header(Header),
    Message(MessageNode),
    Leaf(LeafRecord),
    Summary(SummaryRecord),
    Label(LabelRecord),
}

impl TreeRecord {
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Header(_) => HEADER_TAG,
            Self::Message(_) => MESSAGE_TAG,
            Self::Leaf(_) => LEAF_TAG,
            Self::Summary(_) => SUMMARY_TAG,
            Self::Label(_) => LABEL_TAG,
        }
    }
}

/// Encode one message into a `TreeRecord::Message`, spilling large fields into
/// the payload writer. `parent_id` is the previous node (None for the first);
/// `run_id` and `interrupted` are forwarded for PR-B/D use.
pub fn encode_message<M: StoredInTree>(
    msg: &M,
    parent_id: Option<NodeId>,
    timestamp: u64,
    writer: &mut PayloadWriter,
) -> Result<TreeRecord, SessionError> {
    let content = msg.encode(writer)?;
    let display_text = content
        .get("display_text")
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(TreeRecord::Message(MessageNode {
        id: NodeId::for_message(),
        parent_id,
        role: content
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("user")
            .to_owned(),
        content,
        timestamp,
        run_id: None,
        interrupted: false,
        display_text,
    }))
}

/// Decode a `TreeRecord::Message` back into a concrete message type, paging
/// payload blobs back through the reader.
pub fn decode_message<M: StoredInTree>(
    record: &MessageNode,
    reader: &PayloadReader,
) -> Result<M, SessionError> {
    M::decode(record.content.clone(), reader)
}

/// Index of node id → record position, built on open for O(1) lookup.
#[derive(Debug, Default, Clone)]
pub struct TreeIndex {
    pub nodes: HashMap<String, usize>,
    pub records: Vec<TreeRecord>,
    /// Last leaf by append order (active tip). None if no leaf has been appended.
    pub leaf: Option<LeafRecord>,
}

impl TreeIndex {
    pub fn build(records: Vec<TreeRecord>) -> Self {
        let mut nodes = HashMap::with_capacity(records.len());
        let mut leaf = None;
        for (i, rec) in records.iter().enumerate() {
            let id = match rec {
                TreeRecord::Message(m) => m.id.as_str(),
                TreeRecord::Leaf(l) => {
                    leaf = Some(l.clone());
                    l.id.as_str()
                }
                TreeRecord::Summary(s) => s.id.as_str(),
                TreeRecord::Label(lb) => lb.id.as_str(),
                TreeRecord::Header(_) => continue,
            };
            nodes.insert(id.to_owned(), i);
        }
        Self { nodes, records, leaf }
    }

    pub fn get(&self, id: &str) -> Option<&TreeRecord> {
        self.nodes.get(id).and_then(|&i| self.records.get(i))
    }
}

/// Walk leaf→root resolving the active branch as a list of record indices.
/// In PR-A (linear) the path is simply the chain of messages ending at the leaf
/// target; the cycle guard is present so PR-B branching is safe from the start.
pub fn active_branch_path(index: &TreeIndex) -> Result<Vec<usize>, SessionError> {
    let target = index
        .leaf
        .as_ref()
        .map(|l| l.target_node_id.as_str().to_owned())
        .or_else(|| {
            index
                .records
                .iter()
                .rev()
            .find_map(|r| match r {
                TreeRecord::Message(m) => Some(m.id.as_str().to_owned()),
                _ => None,
            })
        });

    let Some(target_id) = target else {
        return Ok(Vec::new());
    };

    let mut path = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut cur = Some(target_id);
    while let Some(id) = cur {
        if !seen.insert(id.clone()) {
            return Err(SessionError::CorruptTree(format!(
                "cycle at node {id}"
            )));
        }
        let Some(&pos) = index.nodes.get(&id) else {
            return Err(SessionError::MissingNode(id));
        };
        path.push(pos);
        cur = match &index.records[pos] {
            TreeRecord::Message(m) => m.parent_id.as_ref().map(|p| p.as_str().to_owned()),
            TreeRecord::Summary(s) => Some(s.parent_id.as_str().to_owned()),
            TreeRecord::Label(l) => Some(l.parent_id.as_str().to_owned()),
            TreeRecord::Leaf(l) => l.parent_id.as_ref().map(|p| p.as_str().to_owned()),
            TreeRecord::Header(_) => None,
        };
    }
    path.reverse();
    Ok(path)
}

/// Serialize one record to a JSONL line (without trailing newline).
pub fn to_jsonl_line(record: &TreeRecord) -> Result<String, SessionError> {
    serde_json::to_string(record).map_err(StorageError::from).map_err(Into::into)
}

/// Parse one JSONL line into a record. Unparseable lines are returned as None
/// (skip) so a trailing partial record is dropped rather than fatal.
pub fn parse_jsonl_line(line: &str) -> Result<Option<TreeRecord>, SessionError> {
    if line.trim().is_empty() {
        return Ok(None);
    }
    match serde_json::from_str::<TreeRecord>(line) {
        Ok(r) => Ok(Some(r)),
        Err(e) => {
            tracing::warn!(error = %e, "skipping unparseable tree.jsonl line");
            Ok(None)
        }
    }
}

/// Read `tree.jsonl` fully into a `TreeIndex`, discarding a trailing run of
/// unparseable lines (crash safety — a partial flush may corrupt the tail).
pub fn read_tree_index(dir: &Path) -> Result<TreeIndex, SessionError> {
    let path = dir.join(TREE_JSONL);
    let contents = std::fs::read_to_string(&path).map_err(StorageError::from)?;
    let mut records = Vec::new();
    for line in contents.lines() {
        if let Some(rec) = parse_jsonl_line(line)? {
            records.push(rec);
        }
    }
    Ok(TreeIndex::build(records))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_has_message_prefix() {
        let id = NodeId::for_message();
        assert!(id.as_str().starts_with("msg_"));
    }

    #[test]
    fn leaf_id_has_leaf_prefix() {
        let id = NodeId::for_leaf();
        assert!(id.as_str().starts_with("lft_"));
    }

    #[test]
    fn cycle_guard_triggers() {
        let a = NodeId::for_message();
        let b = NodeId::for_message();
        let records = vec![
            TreeRecord::Message(MessageNode {
                id: a.clone(),
                parent_id: Some(b.clone()),
                role: "user".into(),
                content: Value::Null,
                timestamp: 0,
                run_id: None,
                interrupted: false,
                display_text: None,
            }),
            TreeRecord::Message(MessageNode {
                id: b,
                parent_id: Some(a),
                role: "user".into(),
                content: Value::Null,
                timestamp: 0,
                run_id: None,
                interrupted: false,
                display_text: None,
            }),
            TreeRecord::Leaf(LeafRecord {
                id: NodeId::for_leaf(),
                parent_id: None,
                target_node_id: NodeId::for_message(),
            }),
        ];
        let _ = records;
    }
}
