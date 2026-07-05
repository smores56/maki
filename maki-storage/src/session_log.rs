//! Per-session folder layout: append-only `tree.jsonl` + `payloads.jsonl`,
//! atomic `meta.json`, `lineage.json`.
//!
//! ```
//! sessions/<sessionId>/
//! ├── tree.jsonl       (append-only tree records)
//! ├── payloads.jsonl   (large ContentBlock blobs, keyed by payloadId)
//! ├── meta.json        (SessionMeta, atomic in-place rewrite)
//! └── lineage.json     (fork chains)
//! ```
//!
//! Durability rules (§22): cross-file fsync ordering (payloads before tree),
//! partial-line recovery, parent-dir fsync on path-creating operations.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use tracing::warn;

use crate::atomic_write;
use crate::sessions::SessionError;
use crate::tree::{
    Header, Lineage, Node, NodeId, PayloadRecord, SessionMetaFile, TREE_FORMAT_VERSION,
};

pub(crate) const TREE_FILE: &str = "tree.jsonl";
pub(crate) const PAYLOADS_FILE: &str = "payloads.jsonl";
const META_FILE: &str = "meta.json";
const LINEAGE_FILE: &str = "lineage.json";

const UNSUPPORTED_VERSION_MSG: &str = "unsupported session version";

#[derive(Debug)]
pub struct SessionFolder {
    pub header: Header,
    pub nodes: Vec<Node>,
    pub payloads: Vec<PayloadRecord>,
    pub meta: Option<SessionMetaFile>,
    pub lineage: Lineage,
    pub leaf_id: Option<NodeId>,
    dir: PathBuf,
}

impl SessionFolder {
    pub fn create(dir: &Path, header: Header, lineage: Lineage) -> Result<Self, SessionError> {
        fs::create_dir_all(dir).map_err(crate::StorageError::from)?;
        sync_parent_dir_of(dir);

        let tree_path = dir.join(TREE_FILE);
        let tree_header_node = Node::Header(header.clone());
        let mut buf = Vec::new();
        append_jsonl(&mut buf, &tree_header_node)?;
        atomic_write(&tree_path, &buf)?;

        let payloads_path = dir.join(PAYLOADS_FILE);
        File::create(&payloads_path).map_err(crate::StorageError::from)?;
        sync_parent_dir_of(dir);

        let meta = SessionMetaFile {
            title: String::new(),
            token_usage: serde_json::Value::Null,
            updated_at: header.created_at,
            created_at: header.created_at,
            model: None,
            meta: Default::default(),
        };
        write_meta(dir, &meta)?;

        write_lineage(dir, &lineage)?;

        Ok(Self {
            header,
            nodes: vec![tree_header_node],
            payloads: Vec::new(),
            meta: Some(meta),
            lineage,
            leaf_id: None,
            dir: dir.to_path_buf(),
        })
    }

    pub fn open(dir: &Path) -> Result<Self, SessionError> {
        let tree_path = dir.join(TREE_FILE);
        let (header, nodes, leaf_id) = load_tree(&tree_path)?;
        let payloads = load_payloads(&dir.join(PAYLOADS_FILE))?;
        let meta = load_meta(dir);
        let lineage = load_lineage(dir);

        Ok(Self {
            header,
            nodes,
            payloads,
            meta,
            lineage,
            leaf_id,
            dir: dir.to_path_buf(),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn node_by_id(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id() == id)
    }

    pub fn payload_by_id(&self, id: &str) -> Option<&PayloadRecord> {
        self.payloads.iter().find(|p| p.id() == id)
    }

    pub fn payload_for(&self, node_id: &str, block_idx: usize) -> Option<&PayloadRecord> {
        self.payloads.iter().find(|p| {
            p.node_id() == node_id
                && match p {
                    PayloadRecord::ToolResult { block_idx: bi, .. } => *bi == block_idx,
                    PayloadRecord::Image { block_idx: bi, .. } => *bi == block_idx,
                }
        })
    }

    /// Appends a node to `tree.jsonl` with fsync. The node is added to the
    /// in-memory list. Last-by-append-order `Leaf` wins on load.
    pub fn append_node(&mut self, node: Node) -> Result<(), SessionError> {
        if matches!(node, Node::Header(_)) {
            return Err(SessionError::CorruptTree(
                "cannot append a second header".into(),
            ));
        }
        let mut buf = Vec::new();
        append_jsonl(&mut buf, &node)?;
        append_and_sync(&self.dir.join(TREE_FILE), &buf)?;

        if let Node::Leaf(ref leaf) = node {
            self.leaf_id = Some(leaf.id.clone());
        }
        self.nodes.push(node);
        Ok(())
    }

    /// Appends a payload to `payloads.jsonl` with fsync. Must be called BEFORE
    /// `append_node` for the referencing message (§22 cross-file ordering).
    pub fn append_payload(&mut self, payload: PayloadRecord) -> Result<(), SessionError> {
        let mut buf = Vec::new();
        append_jsonl(&mut buf, &payload)?;
        append_and_sync(&self.dir.join(PAYLOADS_FILE), &buf)?;
        self.payloads.push(payload);
        Ok(())
    }

    pub fn write_meta(&mut self, meta: &SessionMetaFile) -> Result<(), SessionError> {
        write_meta(&self.dir, meta)?;
        self.meta = Some(meta.clone());
        Ok(())
    }

    pub fn write_lineage(&mut self, lineage: &Lineage) -> Result<(), SessionError> {
        write_lineage(&self.dir, lineage)?;
        self.lineage = lineage.clone();
        Ok(())
    }

    pub fn active_leaf_target(&self) -> Option<&NodeId> {
        let active_leaf = self.nodes.iter().rev().find(|n| matches!(n, Node::Leaf(_)));
        match active_leaf? {
            Node::Leaf(leaf) => Some(&leaf.target_node_id),
            _ => None,
        }
    }

    pub fn latest_leaf(&self) -> Option<&Node> {
        self.nodes.iter().rev().find(|n| matches!(n, Node::Leaf(_)))
    }

    /// Walks target_node_id → root collecting message nodes. Cycle-guarded
    /// via a visited set (§22). Used by `active_branch()` in PR2.
    pub fn walk_to_root(&self, from_node_id: &str) -> Vec<&Node> {
        let mut visited = HashSet::new();
        let mut path = Vec::new();
        let mut current = from_node_id.to_string();
        while visited.insert(current.clone()) {
            let Some(node) = self.node_by_id(&current) else {
                break;
            };
            if !matches!(node, Node::Header(_)) {
                path.push(node);
            }
            match node.parent_id() {
                Some(pid) => current = pid.to_string(),
                None => break,
            }
        }
        path
    }
}

fn append_jsonl<R: serde::Serialize>(buf: &mut Vec<u8>, record: &R) -> Result<(), SessionError> {
    serde_json::to_writer(&mut *buf, record).map_err(crate::StorageError::from)?;
    buf.push(b'\n');
    Ok(())
}

fn append_and_sync(path: &Path, buf: &[u8]) -> Result<(), SessionError> {
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .map_err(crate::StorageError::from)?;
    file.write_all(buf).map_err(crate::StorageError::from)?;
    file.sync_data().map_err(crate::StorageError::from)?;
    Ok(())
}

/// fsync the parent of `path` so a just-created directory entry survives a
/// crash (§22: parent-dir fsync after path-creating operations).
pub(crate) fn sync_parent_dir_of(path: &Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    #[cfg(unix)]
    {
        if let Ok(f) = File::open(parent) {
            let _ = f.sync_all();
        }
    }
    #[cfg(not(unix))]
    {
        let _ = parent;
    }
}

fn load_tree(path: &Path) -> Result<(Header, Vec<Node>, Option<NodeId>), SessionError> {
    if !path.exists() {
        return Err(crate::StorageError::NotFound(path.display().to_string()).into());
    }
    let reader = BufReader::new(File::open(path).map_err(crate::StorageError::from)?);
    let mut header: Option<Header> = None;
    let mut nodes = Vec::new();
    let mut leaf_id: Option<NodeId> = None;
    let mut last_good_byte = 0u64;
    let mut truncated = false;

    for (idx, line_result) in reader.lines().enumerate() {
        let line = match line_result {
            Ok(l) => l,
            Err(e) => {
                warn!(path = %path.display(), error = %e, line = idx, "stopping at unparseable line");
                truncated = true;
                break;
            }
        };

        if line.is_empty() {
            last_good_byte += 1;
            continue;
        }

        let node: Node = match serde_json::from_str(&line) {
            Ok(n) => n,
            Err(e) => {
                warn!(path = %path.display(), error = %e, line = idx, "skipping unparseable tree record");
                truncated = true;
                break;
            }
        };
        last_good_byte += line.len() as u64 + 1;
        match &node {
            Node::Header(h) => {
                if h.version > TREE_FORMAT_VERSION {
                    return Err(SessionError::UnsupportedVersion {
                        found: h.version,
                        message: UNSUPPORTED_VERSION_MSG.into(),
                    });
                }
                header = Some(h.clone());
            }
            Node::Leaf(l) => leaf_id = Some(l.id.clone()),
            _ => {}
        }
        nodes.push(node);
    }

    if truncated {
        truncate_to_line_boundary(path, last_good_byte)?;
    }

    let header = header
        .ok_or_else(|| SessionError::CorruptTree("missing header record in tree.jsonl".into()))?;

    Ok((header, nodes, leaf_id))
}

fn load_payloads(path: &Path) -> Result<Vec<PayloadRecord>, SessionError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let reader = BufReader::new(File::open(path).map_err(crate::StorageError::from)?);
    let mut payloads = Vec::new();
    let mut last_good_byte = 0u64;
    let mut truncated = false;

    for (idx, line_result) in reader.lines().enumerate() {
        let line = match line_result {
            Ok(l) => l,
            Err(e) => {
                warn!(path = %path.display(), error = %e, line = idx, "stopping at unparseable payload line");
                truncated = true;
                break;
            }
        };

        if line.is_empty() {
            last_good_byte += 1;
            continue;
        }

        match serde_json::from_str::<PayloadRecord>(&line) {
            Ok(p) => {
                payloads.push(p);
                last_good_byte += line.len() as u64 + 1;
            }
            Err(e) => {
                warn!(path = %path.display(), error = %e, line = idx, "skipping unparseable payload record");
                truncated = true;
                break;
            }
        }
    }

    if truncated {
        truncate_to_line_boundary(path, last_good_byte)?;
    }

    Ok(payloads)
}

fn load_meta(dir: &Path) -> Option<SessionMetaFile> {
    let path = dir.join(META_FILE);
    let data = fs::read(&path).ok()?;
    serde_json::from_slice(&data).ok()
}

fn load_lineage(dir: &Path) -> Lineage {
    let path = dir.join(LINEAGE_FILE);
    match fs::read(&path) {
        Ok(data) => serde_json::from_slice(&data).unwrap_or_default(),
        Err(_) => Lineage::default(),
    }
}

fn write_meta(dir: &Path, meta: &SessionMetaFile) -> Result<(), SessionError> {
    let data = serde_json::to_vec_pretty(meta).map_err(crate::StorageError::from)?;
    atomic_write(&dir.join(META_FILE), &data)?;
    Ok(())
}

fn write_lineage(dir: &Path, lineage: &Lineage) -> Result<(), SessionError> {
    let data = serde_json::to_vec_pretty(lineage).map_err(crate::StorageError::from)?;
    atomic_write(&dir.join(LINEAGE_FILE), &data)?;
    Ok(())
}

fn truncate_to_line_boundary(path: &Path, keep_bytes: u64) -> Result<(), SessionError> {
    let file = OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(crate::StorageError::from)?;
    file.set_len(keep_bytes)
        .map_err(crate::StorageError::from)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::{LeafEntry, MessageNode};
    use maki_util::EntityId;
    use tempfile::TempDir;

    const TEST_CWD: &str = "/test";
    const TIMESTAMP: u64 = 1000;

    fn make_header() -> Header {
        Header {
            version: TREE_FORMAT_VERSION,
            session_id: EntityId::generate(),
            cwd: TEST_CWD.into(),
            created_at: TIMESTAMP,
            parent_session_id: None,
        }
    }

    fn make_message(id: &str, parent_id: &str) -> MessageNode {
        MessageNode {
            id: id.into(),
            parent_id: parent_id.into(),
            role: "user".into(),
            content_blocks: vec![],
            timestamp: TIMESTAMP,
            run_id: None,
        }
    }

    #[test]
    fn create_and_open_round_trips_header() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("session_1");
        let header = make_header();

        let folder = SessionFolder::create(&dir, header.clone(), Lineage::default()).unwrap();
        assert_eq!(folder.header.session_id, header.session_id);
        assert_eq!(folder.nodes.len(), 1);

        let reopened = SessionFolder::open(&dir).unwrap();
        assert_eq!(reopened.header.session_id, header.session_id);
        assert_eq!(reopened.nodes.len(), 1);
        assert!(reopened.leaf_id.is_none());
    }

    #[test]
    fn append_message_and_leaf_round_trips() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("s");
        let header = make_header();
        let session_id = header.session_id.to_string();

        let mut folder = SessionFolder::create(&dir, header, Lineage::default()).unwrap();
        let msg = make_message("msg_1", &session_id);
        folder.append_node(Node::Message(msg.clone())).unwrap();

        let leaf = Node::Leaf(LeafEntry {
            id: "lft_1".into(),
            parent_id: session_id,
            target_node_id: "msg_1".into(),
        });
        folder.append_node(leaf).unwrap();
        assert_eq!(folder.leaf_id.as_deref(), Some("lft_1"));

        let reopened = SessionFolder::open(&dir).unwrap();
        assert_eq!(reopened.nodes.len(), 3);
        assert_eq!(reopened.leaf_id.as_deref(), Some("lft_1"));
    }

    #[test]
    fn last_leaf_by_append_order_wins() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("s");
        let header = make_header();
        let session_id = header.session_id.to_string();

        let mut folder = SessionFolder::create(&dir, header, Lineage::default()).unwrap();
        folder
            .append_node(Node::Message(make_message("msg_1", &session_id)))
            .unwrap();
        folder
            .append_node(Node::Leaf(LeafEntry {
                id: "lft_1".into(),
                parent_id: session_id,
                target_node_id: "msg_1".into(),
            }))
            .unwrap();
        folder
            .append_node(Node::Message(make_message("msg_2", "msg_1")))
            .unwrap();
        folder
            .append_node(Node::Leaf(LeafEntry {
                id: "lft_2".into(),
                parent_id: "lft_1".into(),
                target_node_id: "msg_2".into(),
            }))
            .unwrap();

        let reopened = SessionFolder::open(&dir).unwrap();
        let target = reopened.active_leaf_target().unwrap();
        assert_eq!(target, "msg_2");
    }

    #[test]
    fn walk_to_root_collects_chain() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("s");
        let header = make_header();
        let session_id = header.session_id.to_string();

        let mut folder = SessionFolder::create(&dir, header, Lineage::default()).unwrap();
        folder
            .append_node(Node::Message(make_message("msg_1", &session_id)))
            .unwrap();
        folder
            .append_node(Node::Message(make_message("msg_2", "msg_1")))
            .unwrap();
        folder
            .append_node(Node::Message(make_message("msg_3", "msg_2")))
            .unwrap();

        let path = folder.walk_to_root("msg_3");
        assert_eq!(path.len(), 3);
        assert_eq!(path[0].id(), "msg_3");
        assert_eq!(path[2].id(), "msg_1");
    }

    #[test]
    fn walk_to_root_cycle_guard_terminates() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("s");
        let header = make_header();

        let mut folder = SessionFolder::create(&dir, header, Lineage::default()).unwrap();
        folder
            .append_node(Node::Message(make_message("msg_a", "msg_b")))
            .unwrap();
        folder
            .append_node(Node::Message(make_message("msg_b", "msg_a")))
            .unwrap();

        let path = folder.walk_to_root("msg_a");
        assert_eq!(path.len(), 2);
    }

    #[test]
    fn append_payload_and_lookup() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("s");
        let header = make_header();

        let mut folder = SessionFolder::create(&dir, header, Lineage::default()).unwrap();
        let payload = PayloadRecord::ToolResult {
            id: "pld_1".into(),
            node_id: "msg_1".into(),
            block_idx: 0,
            content: "result data".into(),
            is_error: false,
        };
        folder.append_payload(payload).unwrap();

        let reopened = SessionFolder::open(&dir).unwrap();
        let found = reopened.payload_for("msg_1", 0).unwrap();
        assert_eq!(found.id(), "pld_1");
    }

    #[test]
    fn write_and_load_meta() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("s");
        let header = make_header();

        let mut folder = SessionFolder::create(&dir, header, Lineage::default()).unwrap();
        let meta = SessionMetaFile {
            title: "Test Title".into(),
            token_usage: serde_json::json!({"input": 500}),
            updated_at: TIMESTAMP,
            created_at: 0,
            model: Some("claude".into()),
            meta: Default::default(),
        };
        folder.write_meta(&meta).unwrap();

        let reopened = SessionFolder::open(&dir).unwrap();
        let loaded = reopened.meta.unwrap();
        assert_eq!(loaded.title, "Test Title");
        assert_eq!(loaded.model, Some("claude".into()));
    }

    #[test]
    fn write_and_load_lineage() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("s");
        let header = make_header();

        let mut folder = SessionFolder::create(&dir, header, Lineage::default()).unwrap();
        let lineage = Lineage {
            parent_session_id: Some(EntityId::generate()),
            created_from_node_id: Some("msg_5".into()),
        };
        folder.write_lineage(&lineage).unwrap();

        let reopened = SessionFolder::open(&dir).unwrap();
        assert_eq!(
            reopened.lineage.parent_session_id,
            lineage.parent_session_id
        );
        assert_eq!(
            reopened.lineage.created_from_node_id.as_deref(),
            Some("msg_5")
        );
    }

    #[test]
    fn partial_line_recovery_truncates_corrupt_tail() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("s");
        let header = make_header();
        let session_id = header.session_id.to_string();

        let mut folder = SessionFolder::create(&dir, header, Lineage::default()).unwrap();
        folder
            .append_node(Node::Message(make_message("msg_1", &session_id)))
            .unwrap();

        drop(folder);
        let tree_path = dir.join(TREE_FILE);
        {
            let mut file = OpenOptions::new().append(true).open(&tree_path).unwrap();
            writeln!(file, "{{THIS_IS_NOT_VALID_JSON").unwrap();
            file.sync_data().unwrap();
        }

        let reopened = SessionFolder::open(&dir).unwrap();
        assert_eq!(reopened.nodes.len(), 2);
        assert_eq!(reopened.nodes[1].id(), "msg_1");
        drop(reopened);

        {
            let mut file = OpenOptions::new().append(true).open(&tree_path).unwrap();
            writeln!(file, "{{ALSO_NOT_JSON").unwrap();
            file.sync_data().unwrap();
        }

        match SessionFolder::open(&dir) {
            Ok(again) => assert_eq!(again.nodes.len(), 2),
            Err(e) => panic!("re-opening after re-corruption failed: {e}"),
        }
    }

    #[test]
    fn unsupported_version_rejected() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("s");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(TREE_FILE);
        let bad_header = Node::Header(Header {
            version: 99,
            session_id: EntityId::generate(),
            cwd: TEST_CWD.into(),
            created_at: 0,
            parent_session_id: None,
        });
        let mut buf = Vec::new();
        serde_json::to_writer(&mut buf, &bad_header).unwrap();
        buf.push(b'\n');
        fs::write(&path, buf).unwrap();

        let err = SessionFolder::open(&dir).unwrap_err();
        assert!(matches!(
            err,
            SessionError::UnsupportedVersion { found: 99, .. }
        ));
    }

    #[test]
    fn cannot_append_second_header() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("s");
        let header = make_header();
        let mut folder = SessionFolder::create(&dir, header, Lineage::default()).unwrap();
        let err = folder
            .append_node(Node::Header(Header {
                version: TREE_FORMAT_VERSION,
                session_id: EntityId::generate(),
                cwd: TEST_CWD.into(),
                created_at: 0,
                parent_session_id: None,
            }))
            .unwrap_err();
        assert!(matches!(err, SessionError::CorruptTree(_)));
    }

    #[test]
    fn delete_session_folder_removes_directory() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("sessions");
        let session_dir = dir.join("s");
        let header = make_header();
        let _folder = SessionFolder::create(&session_dir, header, Lineage::default()).unwrap();

        assert!(session_dir.exists());
        fs::remove_dir_all(&session_dir).unwrap();
        assert!(!session_dir.exists());
    }
}
