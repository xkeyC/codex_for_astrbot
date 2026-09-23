//! Fork addition: tests for the memory-file tools a consolidation agent uses
//! when the host gives Codex no execution environment.

use std::sync::Arc;

use codex_extension_api::ConversationHistory;
use codex_extension_api::NoopTurnItemEmitter;
use codex_extension_api::ToolCall;
use codex_extension_api::ToolCallSource;
use codex_extension_api::ToolExecutor;
use codex_extension_api::ToolName;
use codex_extension_api::ToolPayload;
use codex_utils_output_truncation::TruncationPolicy;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

use crate::LIST_TOOL_NAME;
use crate::MEMORY_TOOLS_NAMESPACE;
use crate::READ_TOOL_NAME;
use crate::WRITE_TOOL_NAME;
use crate::maintenance::MaintenanceBackend;
use crate::tools::maintenance_tools;

type MemoryTool = Arc<dyn for<'call> ToolExecutor<ToolCall<'call>>>;

fn tool_name(name: &str) -> ToolName {
    ToolName::namespaced(MEMORY_TOOLS_NAMESPACE, name)
}

fn tools_for(root: &std::path::Path) -> Vec<MemoryTool> {
    maintenance_tools(MaintenanceBackend::new(root.to_path_buf()), None)
}

async fn call(tools: &[MemoryTool], name: &str, arguments: Value) -> Result<(), String> {
    let name = tool_name(name);
    let tool = tools
        .iter()
        .find(|tool| tool.tool_name() == name)
        .expect("tool is registered");
    tool.handle(ToolCall {
        turn_id: "turn-1".to_string(),
        call_id: "call-1".to_string(),
        tool_name: name,
        model: "gpt-test".to_string(),
        codex_turn_metadata: None,
        scopes: Vec::new(),
        truncation_policy: TruncationPolicy::Bytes(4096),
        source: ToolCallSource::Direct,
        conversation_history: ConversationHistory::default(),
        turn_item_emitter: Arc::new(NoopTurnItemEmitter),
        environments: Vec::new(),
        payload: ToolPayload::Function {
            arguments: arguments.to_string(),
        },
    })
    .await
    .map(|_| ())
    .map_err(|err| err.to_string())
}

#[tokio::test]
async fn offers_exactly_list_read_and_write() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let names: Vec<String> = tools_for(tempdir.path())
        .iter()
        .map(|tool| tool.tool_name().to_string())
        .collect();

    assert_eq!(
        names,
        vec![
            tool_name(LIST_TOOL_NAME).to_string(),
            tool_name(READ_TOOL_NAME).to_string(),
            tool_name(WRITE_TOOL_NAME).to_string(),
        ],
        "consolidation needs no ad-hoc notes and no search"
    );
}

#[tokio::test]
async fn write_creates_then_replaces_a_file() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let tools = tools_for(tempdir.path());
    let summary = tempdir.path().join("memory_summary.md");

    call(
        &tools,
        WRITE_TOOL_NAME,
        json!({ "path": "memory_summary.md", "content": "v1\nfirst\n" }),
    )
    .await
    .expect("create");
    assert_eq!(
        tokio::fs::read_to_string(&summary).await.expect("read"),
        "v1\nfirst\n"
    );

    // A whole-file replacement, which is what consolidation does every run.
    call(
        &tools,
        WRITE_TOOL_NAME,
        json!({ "path": "memory_summary.md", "content": "v1\nsecond\n" }),
    )
    .await
    .expect("replace");
    assert_eq!(
        tokio::fs::read_to_string(&summary).await.expect("read"),
        "v1\nsecond\n"
    );
}

#[tokio::test]
async fn write_creates_missing_parent_directories() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let tools = tools_for(tempdir.path());

    call(
        &tools,
        WRITE_TOOL_NAME,
        json!({ "path": "extensions/notes/topic.md", "content": "note\n" }),
    )
    .await
    .expect("write");

    assert_eq!(
        tokio::fs::read_to_string(tempdir.path().join("extensions/notes/topic.md"))
            .await
            .expect("read"),
        "note\n"
    );
}

#[tokio::test]
async fn write_accepts_the_absolute_path_the_prompt_uses() {
    // The consolidation prompt names files as `<memory root>/memory_summary.md`.
    let tempdir = tempfile::tempdir().expect("tempdir");
    let tools = tools_for(tempdir.path());
    let absolute = tempdir.path().join("memory_summary.md");

    call(
        &tools,
        WRITE_TOOL_NAME,
        json!({ "path": absolute.to_string_lossy(), "content": "v1\n" }),
    )
    .await
    .expect("write through an absolute path");

    assert_eq!(
        tokio::fs::read_to_string(&absolute).await.expect("read"),
        "v1\n"
    );
}

#[tokio::test]
async fn write_refuses_to_leave_the_memory_root() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let root = tempdir.path().join("memories");
    tokio::fs::create_dir_all(&root).await.expect("mkdir");
    let tools = tools_for(&root);
    let outside = tempdir.path().join("escaped.md");

    for path in [
        "../escaped.md".to_string(),
        outside.to_string_lossy().to_string(),
    ] {
        let err = call(
            &tools,
            WRITE_TOOL_NAME,
            json!({ "path": path, "content": "nope" }),
        )
        .await
        .expect_err("must be refused");
        assert!(
            err.contains("memories root"),
            "unexpected error for {path}: {err}"
        );
    }
    assert!(!outside.exists());
}

#[tokio::test]
async fn read_sees_what_write_wrote() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let tools = tools_for(tempdir.path());

    call(
        &tools,
        WRITE_TOOL_NAME,
        json!({ "path": "memory_summary.md", "content": "v1\nkept\n" }),
    )
    .await
    .expect("write");

    call(
        &tools,
        READ_TOOL_NAME,
        json!({ "path": "memory_summary.md" }),
    )
    .await
    .expect("read");
    call(&tools, LIST_TOOL_NAME, json!({})).await.expect("list");
}
