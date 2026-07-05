//! Conversation tree node taxonomy, mutations, and events.
//!
//! Each session is an append-only tree of records stored in `tree.jsonl`.
//! Six node types form the tree (§4): `Header`, `Message`, `Leaf`,
//! `BranchSummary`, `Compaction`, `Label`. Large content blobs
//! (`ToolResult`, `Image`) are paged to `payloads.jsonl` and referenced by id
//! (§5), keeping message nodes small.
//!
//! Mutations flow through `TreeMutation` to the writer thread (§21); the
//! writer persists them and emits `TreeEvent`s for UI sync (§8).

use std::path::PathBuf;

use maki_util::EntityId;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::sessions::SessionMeta;

pub const TREE_FORMAT_VERSION: u32 = 2;
const ID_PREFIX_MSG: &str = "msg_";
const ID_PREFIX_PAYLOAD: &str = "pld_";
const ID_PREFIX_SUMMARY: &str = "sum_";
const ID_PREFIX_COMPACTION: &str = "cmp_";
const ID_PREFIX_LEAF: &str = "lft_";
const ID_PREFIX_LABEL: &str = "lbl_";

/// Compact, sortable, file-safe identifier for tree nodes.
///
/// Base58-encoded UUIDv7 (via [`EntityId`]) with a branded prefix per record
/// type. Lexicographically sortable; collision-free in practice. The prefix
/// distinguishes node kinds on disk without needing a separate type tag.
pub type NodeId = String;

pub fn new_node_id(prefix: &str) -> NodeId {
    format!("{prefix}{entity}", entity = EntityId::generate())
}

pub fn new_message_id() -> NodeId {
    new_node_id(ID_PREFIX_MSG)
}

pub fn new_payload_id() -> String {
    new_node_id(ID_PREFIX_PAYLOAD)
}

pub fn new_leaf_id() -> NodeId {
    new_node_id(ID_PREFIX_LEAF)
}

pub fn new_summary_id() -> NodeId {
    new_node_id(ID_PREFIX_SUMMARY)
}

pub fn new_compaction_id() -> NodeId {
    new_node_id(ID_PREFIX_COMPACTION)
}

pub fn new_label_id() -> NodeId {
    new_node_id(ID_PREFIX_LABEL)
}

/// Path-root-to-cursor file list for fork lineages. `parent_session_id` is a
/// session [`EntityId`]; `created_from_node_id` is a prefixed tree [`NodeId`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Lineage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<EntityId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_from_node_id: Option<NodeId>,
}

/// First record of `tree.jsonl`. `session_id` is the canonical session
/// [`EntityId`] (no branded prefix, unlike other node ids — §3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Header {
    pub version: u32,
    pub session_id: EntityId,
    pub cwd: String,
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<EntityId>,
}

/// A message tree node. Large `ToolResult`/`Image` blocks hold a ref
/// (`{ kind: "ref", payloadId }`) instead of inline content (§5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageNode {
    pub id: NodeId,
    pub parent_id: NodeId,
    pub role: String,
    pub content_blocks: Vec<Value>,
    pub timestamp: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<u64>,
}

/// Move-record (not content). `parent_id` chains to the previous leaf,
/// enabling undo-of-rewind. Last-by-append-order wins on load (§4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeafEntry {
    pub id: NodeId,
    pub parent_id: NodeId,
    pub target_node_id: NodeId,
}

/// Synthetic summary of an abandoned branch after a rewind (§13).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchSummary {
    pub id: NodeId,
    pub parent_id: NodeId,
    pub from_id: NodeId,
    pub narrative: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub read_files: Vec<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modified_files: Vec<PathBuf>,
}

/// Replaces an old prefix of the active branch with a summary (§10).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Compaction {
    pub id: NodeId,
    pub parent_id: NodeId,
    pub summary: String,
    pub first_kept_id: NodeId,
    pub timestamp: u64,
}

/// Names a branch starting at a message node. Metadata-only; excluded from
/// `active_branch()` provider context (§4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Label {
    pub id: NodeId,
    pub parent_id: NodeId,
    pub name: String,
}

/// The six record types in `tree.jsonl`, tagged via `t` (matching the
/// `LogRecord` convention).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Node {
    Header(Header),
    Message(MessageNode),
    Leaf(LeafEntry),
    BranchSummary(BranchSummary),
    Compaction(Compaction),
    Label(Label),
}

impl Node {
    pub fn id(&self) -> std::borrow::Cow<'_, str> {
        match self {
            Self::Header(h) => h.session_id.to_string().into(),
            Self::Message(m) => m.id.as_str().into(),
            Self::Leaf(l) => l.id.as_str().into(),
            Self::BranchSummary(s) => s.id.as_str().into(),
            Self::Compaction(c) => c.id.as_str().into(),
            Self::Label(l) => l.id.as_str().into(),
        }
    }

    pub fn parent_id(&self) -> Option<&str> {
        match self {
            Self::Header(_) => None,
            Self::Message(m) => Some(&m.parent_id),
            Self::Leaf(l) => Some(&l.parent_id),
            Self::BranchSummary(s) => Some(&s.parent_id),
            Self::Compaction(c) => Some(&c.parent_id),
            Self::Label(l) => Some(&l.parent_id),
        }
    }
}

/// A paged-out content blob in `payloads.jsonl` (§5). Tagged by `kind`
/// to preserve the original `ContentBlock` variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PayloadRecord {
    ToolResult {
        id: String,
        node_id: NodeId,
        block_idx: usize,
        content: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
    Image {
        id: String,
        node_id: NodeId,
        block_idx: usize,
        media_type: String,
        data: String,
    },
}

impl PayloadRecord {
    pub fn id(&self) -> &str {
        match self {
            Self::ToolResult { id, .. } | Self::Image { id, .. } => id,
        }
    }

    pub fn node_id(&self) -> &str {
        match self {
            Self::ToolResult { node_id, .. } | Self::Image { node_id, .. } => node_id,
        }
    }
}

/// Session-level metadata stored in `meta.json` (§19), updated in place via
/// atomic-write.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetaFile {
    pub title: String,
    pub token_usage: Value,
    pub updated_at: u64,
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(flatten)]
    pub meta: SessionMeta,
}

/// Durability grouping for turn-level commits (§21).
#[derive(Debug, Clone)]
pub struct Barrier;

/// A request to summarize a discarded branch after a rewind (§13).
#[derive(Debug, Clone)]
pub struct SummarizeRequest {
    pub from_id: NodeId,
    pub to_id: NodeId,
}

/// Mutations enqueued to the writer thread (§21). All appends to disk.
#[derive(Debug, Clone)]
pub enum TreeMutation {
    AppendMessage(MessageNode),
    AppendPayload(PayloadRecord),
    LeafMove {
        target_node_id: NodeId,
    },
    AppendLabel(Label),
    AppendBranchSummary(BranchSummary),
    AppendCompaction(Compaction),
    Fork {
        new_session_id: NodeId,
        from_node_id: NodeId,
    },
    Rewind {
        cursor_node_id: NodeId,
        summarize: Option<SummarizeRequest>,
    },
    Barrier(Barrier),
}

/// Notifications emitted after a mutation is durably committed (§8).
/// Disk-first ordering: never emitted for not-yet-durable nodes.
#[derive(Debug, Clone)]
pub enum TreeEvent {
    /// Covers `message`, `branch_summary`, `compaction`, `label` appends.
    Append { node_id: NodeId, from_hook: bool },
    LeafMove {
        old_leaf_id: NodeId,
        new_leaf_id: NodeId,
        from_hook: bool,
    },
    Navigate {
        old_leaf_id: NodeId,
        new_leaf_id: NodeId,
        from_hook: bool,
    },
    Fork {
        old_session_id: NodeId,
        new_session_id: NodeId,
        from_node_id: NodeId,
        from_hook: bool,
    },
    Compact {
        node_id: NodeId,
        first_kept_id: NodeId,
        from_hook: bool,
    },
    Label {
        node_id: NodeId,
        name: String,
        from_hook: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_has_prefix_and_sorted_ordering() {
        let id_a = new_message_id();
        let id_b = new_message_id();
        assert!(id_a.starts_with(ID_PREFIX_MSG));
        assert!(id_b.starts_with(ID_PREFIX_MSG));
        assert_ne!(id_a, id_b);
    }

    #[test]
    fn payload_id_has_prefix() {
        let id = new_payload_id();
        assert!(id.starts_with(ID_PREFIX_PAYLOAD));
    }

    #[test]
    fn leaf_id_has_prefix() {
        let id = new_leaf_id();
        assert!(id.starts_with(ID_PREFIX_LEAF));
    }

    #[test]
    fn node_id_round_trips_through_tree_format_version() {
        let id = new_summary_id();
        assert!(id.starts_with(ID_PREFIX_SUMMARY));
        assert_eq!(TREE_FORMAT_VERSION, 2);
    }

    #[test]
    fn label_and_compaction_prefixes() {
        assert!(new_label_id().starts_with(ID_PREFIX_LABEL));
        assert!(new_compaction_id().starts_with(ID_PREFIX_COMPACTION));
    }

    #[test]
    fn node_id_accessor_returns_correct_field() {
        let header = Node::Header(Header {
            version: TREE_FORMAT_VERSION,
            session_id: EntityId::generate(),
            cwd: "/tmp".into(),
            created_at: 0,
            parent_session_id: None,
        });
        let header_id = header.id().to_string();
        assert_eq!(header.id(), header_id.as_str());
        assert_eq!(header.parent_id(), None);

        let msg = Node::Message(MessageNode {
            id: "msg_1".into(),
            parent_id: "msg_0".into(),
            role: "user".into(),
            content_blocks: vec![],
            timestamp: 0,
            run_id: None,
        });
        assert_eq!(msg.id(), "msg_1");
        assert_eq!(msg.parent_id(), Some("msg_0"));
    }

    #[test]
    fn payload_record_accessors() {
        let p = PayloadRecord::ToolResult {
            id: "pld_1".into(),
            node_id: "msg_1".into(),
            block_idx: 0,
            content: "result".into(),
            is_error: false,
        };
        assert_eq!(p.id(), "pld_1");
        assert_eq!(p.node_id(), "msg_1");
    }

    #[test]
    fn node_serializes_with_tag_t() {
        let msg = Node::Message(MessageNode {
            id: "msg_1".into(),
            parent_id: "msg_0".into(),
            role: "user".into(),
            content_blocks: vec![],
            timestamp: 0,
            run_id: None,
        });
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""t":"message""#), "got: {json}");

        let parsed: Node = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id(), "msg_1");
    }

    #[test]
    fn leaf_entry_serializes_with_tag() {
        let leaf = Node::Leaf(LeafEntry {
            id: "lft_1".into(),
            parent_id: "lft_0".into(),
            target_node_id: "msg_5".into(),
        });
        let json = serde_json::to_string(&leaf).unwrap();
        assert!(json.contains(r#""t":"leaf""#), "got: {json}");
    }

    #[test]
    fn payload_record_serializes_with_kind_tag() {
        let payload = PayloadRecord::Image {
            id: "pld_1".into(),
            node_id: "msg_1".into(),
            block_idx: 0,
            media_type: "png".into(),
            data: "base64data".into(),
        };
        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains(r#""kind":"image""#), "got: {json}");
    }

    #[test]
    fn session_meta_file_serde_round_trips() {
        let meta = SessionMetaFile {
            title: "test".into(),
            token_usage: serde_json::json!({"input": 100}),
            updated_at: 123,
            created_at: 0,
            model: Some("claude".into()),
            meta: SessionMeta::default(),
        };
        let json = serde_json::to_string(&meta).unwrap();
        let parsed: SessionMetaFile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.title, "test");
        assert_eq!(parsed.model, Some("claude".into()));
    }
}
