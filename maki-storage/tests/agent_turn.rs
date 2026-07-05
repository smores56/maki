//! Integration test: a full agent turn driven through the new tree storage,
//! exercising the exact node sequence the agent loop will emit once wired
//! (PR2). Validates PR1's storage layer in isolation.
//!
//! Sequence per turn (§4, §5, §22):
//!   Header → Message(user) → Message(assistant w/ tool_use + Ref)
//!          → Payload(tool_result) [appended BEFORE the referencing message
//!             would be, in a real turn — here we append after to keep the
//!             test deterministic; cross-file ordering only matters on crash]
//!          → Leaf(target = assistant)
//!
//! Then: reopen from disk, walk_to_root, resolve every payload ref, verify
//! durability bits (files exist, header is singular, leaf points correctly).

use maki_storage::session_log::SessionFolder;
use maki_storage::sessions::SessionMeta;
use maki_storage::tree::{
    Header, LeafEntry, Lineage, MessageNode, Node, PayloadRecord, SessionMetaFile,
    TREE_FORMAT_VERSION, new_leaf_id, new_message_id, new_payload_id,
};
use serde_json::json;
use tempfile::TempDir;

const TEST_CWD: &str = "/test";
const TEST_SESSION_ID: &str = "test-session-1";
const CREATED_AT: u64 = 1_700_000_000;
const ROLE_USER: &str = "user";
const ROLE_ASSISTANT: &str = "assistant";
const TOOL_USE_ID: &str = "toolu_abc";
const PAYLOAD_CONTENT: &str = "tool output body";
const PAYLOAD_BLOCK_IDX: usize = 1;

fn make_header() -> Header {
    Header {
        version: TREE_FORMAT_VERSION,
        session_id: TEST_SESSION_ID.to_string(),
        cwd: TEST_CWD.to_string(),
        created_at: CREATED_AT,
        parent_session_id: None,
    }
}

fn user_message(parent_id: &str) -> MessageNode {
    MessageNode {
        id: new_message_id(),
        parent_id: parent_id.to_string(),
        role: ROLE_USER.to_string(),
        content_blocks: vec![json!({
            "type": "text",
            "text": "What did we do so far?",
        })],
        timestamp: CREATED_AT + 1,
        run_id: Some(1),
    }
}

fn assistant_message_with_ref(parent_id: &str, payload_id: &str) -> MessageNode {
    MessageNode {
        id: new_message_id(),
        parent_id: parent_id.to_string(),
        role: ROLE_ASSISTANT.to_string(),
        content_blocks: vec![
            json!({"type": "text", "text": "Running a tool."}),
            json!({"type": "tool_use", "id": TOOL_USE_ID, "name": "bash"}),
            json!({"kind": "ref", "payloadId": payload_id}),
        ],
        timestamp: CREATED_AT + 2,
        run_id: Some(1),
    }
}

fn tool_result_payload(node_id: &str, payload_id: String) -> PayloadRecord {
    PayloadRecord::ToolResult {
        id: payload_id,
        node_id: node_id.to_string(),
        block_idx: PAYLOAD_BLOCK_IDX,
        content: PAYLOAD_CONTENT.to_string(),
        is_error: false,
    }
}

fn leaf(parent_id: &str, target: &str) -> LeafEntry {
    LeafEntry {
        id: new_leaf_id(),
        parent_id: parent_id.to_string(),
        target_node_id: target.to_string(),
    }
}

#[test]
fn full_agent_turn_round_trips_through_session_folder() {
    let tmp = TempDir::new().unwrap();
    let session_dir = tmp.path().join(TEST_SESSION_ID);

    let mut folder = SessionFolder::create(&session_dir, make_header(), Lineage::default())
        .expect("create session folder");

    let header_id = folder.header.session_id.clone();

    // Turn 1: user → assistant(tool_use) → tool_result → leaf.
    let user = user_message(&header_id);
    let user_id = user.id.clone();
    folder.append_node(Node::Message(user)).unwrap();

    let payload_id = new_payload_id();
    let assistant = assistant_message_with_ref(&user_id, &payload_id);
    let assistant_id = assistant.id.clone();
    // §22: payloads must be durable before the referencing tree node.
    folder
        .append_payload(tool_result_payload(&assistant_id, payload_id.clone()))
        .unwrap();
    folder.append_node(Node::Message(assistant)).unwrap();

    let leaf = leaf(&assistant_id, &assistant_id);
    let leaf_id = leaf.id.clone();
    folder.append_node(Node::Leaf(leaf)).unwrap();

    // Turn 2: branching from the leaf's target (assistant), a second user msg.
    let user2 = user_message(&assistant_id);
    let user2_id = user2.id.clone();
    folder.append_node(Node::Message(user2)).unwrap();

    folder
        .write_meta(&SessionMetaFile {
            title: "integration".to_string(),
            token_usage: json!({"input": 10, "output": 20}),
            updated_at: CREATED_AT + 10,
            created_at: CREATED_AT,
            model: Some("claude".to_string()),
            meta: SessionMeta::default(),
        })
        .unwrap();

    // Reopen from disk — durability check.
    let reopened = SessionFolder::open(&session_dir).expect("reopen session folder");

    assert_eq!(reopened.header.session_id, TEST_SESSION_ID);
    assert_eq!(reopened.header.version, TREE_FORMAT_VERSION);
    assert_eq!(reopened.header.cwd, TEST_CWD);

    let header_count = reopened
        .nodes
        .iter()
        .filter(|n| matches!(n, Node::Header(_)))
        .count();
    assert_eq!(header_count, 1, "exactly one header after reopen");

    // active_leaf_target points at the latest message (assistant of turn 1).
    let active = reopened
        .active_leaf_target()
        .expect("active leaf target exists");
    assert_eq!(active.as_str(), assistant_id);

    // walk_to_root from user2 → assistant → user → header (header excluded).
    let path = reopened.walk_to_root(&user2_id);
    assert!(!path.is_empty(), "walk_to_root non-empty");
    let path_ids: Vec<&str> = path.iter().map(|n| n.id()).collect();
    assert_eq!(path_ids[0], user2_id);
    assert_eq!(path_ids[1], assistant_id);
    assert_eq!(path_ids[2], user_id);
    // No header in the path (§4 walk excludes Header).
    assert!(
        !path_ids.iter().any(|id| *id == header_id),
        "header must not appear in walk_to_root"
    );

    // Cycle guard: no duplicate ids in the walk.
    let mut sorted = path_ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), path_ids.len(), "walk has no cycles");

    // Payload ref resolves via payload_by_id AND payload_for.
    let by_id = reopened
        .payload_by_id(&payload_id)
        .expect("payload resolvable by id");
    assert_eq!(by_id.node_id(), assistant_id);

    // Every ref block in every message must resolve.
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
                .expect("ref has payloadId");
            assert!(
                reopened.payload_by_id(pid).is_some()
                    || reopened.payload_for(m.id.as_str(), idx).is_some(),
                "unresolved payload ref {pid} on message {}",
                m.id
            );
        }
    }

    // latest_leaf is the one we appended.
    let latest_leaf = reopened.latest_leaf().expect("a leaf exists");
    match latest_leaf {
        Node::Leaf(l) => {
            assert_eq!(l.id, leaf_id);
            assert_eq!(l.target_node_id, assistant_id);
        }
        _ => unreachable!(),
    }
}

#[test]
fn partial_line_recovery_after_truncated_write() {
    let tmp = TempDir::new().unwrap();
    let session_dir = tmp.path().join("truncated");

    let mut folder =
        SessionFolder::create(&session_dir, make_header(), Lineage::default()).expect("create");
    let header_id = folder.header.session_id.clone();
    let user = user_message(&header_id);
    folder.append_node(Node::Message(user)).unwrap();

    // Append a truncated (invalid) line to tree.jsonl simulating a crash mid-write.
    let tree_path = session_dir.join("tree.jsonl");
    let valid_len = std::fs::metadata(&tree_path).unwrap().len();
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(&tree_path)
        .unwrap();
    f.write_all(b"{\"t\":\"message\",\"id\":\"broken\",\"parent_id\":\"x\",\"role\":\"user\"")
        .unwrap();
    drop(f);

    let reopened = SessionFolder::open(&session_dir).expect("reopen recovers valid prefix");
    assert_eq!(
        reopened.nodes.len(),
        2,
        "header + 1 valid message recovered"
    );
    assert_eq!(
        std::fs::metadata(&tree_path).unwrap().len(),
        valid_len,
        "truncated suffix dropped on recovery"
    );
}
