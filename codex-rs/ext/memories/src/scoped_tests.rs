use std::sync::Arc;

use codex_extension_api::ConversationHistory;
use codex_extension_api::NoopTurnItemEmitter;
use codex_extension_api::ToolCallSource;
use codex_extension_api::ToolName;
use codex_extension_api::ToolPayload;
use codex_utils_absolute_path::test_support::PathExt;
use codex_utils_output_truncation::TruncationPolicy;
use pretty_assertions::assert_eq;
use serde_json::json;

use super::*;
use crate::backend::SearchMatchMode;

struct Fixture {
    _tempdir: tempfile::TempDir,
    codex_home: AbsolutePathBuf,
    global_root: std::path::PathBuf,
    local_root: std::path::PathBuf,
    other_root: std::path::PathBuf,
}

async fn fixture() -> Fixture {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let codex_home = tempdir.path().abs();
    let global_root = tempdir.path().join("memories");
    let local_root = scope_root(&codex_home, MemoryVersion::V1, "chat-1").to_path_buf();
    let other_root = scope_root(&codex_home, MemoryVersion::V1, "chat-2").to_path_buf();
    for (root, text) in [
        (&global_root, "shared fact"),
        (&local_root, "alice likes tea"),
        (&other_root, "bob secret"),
    ] {
        tokio::fs::create_dir_all(root).await.expect("mkdir");
        tokio::fs::write(root.join("MEMORY.md"), format!("{text}\n"))
            .await
            .expect("write memory");
        tokio::fs::write(root.join("memory_summary.md"), format!("summary: {text}"))
            .await
            .expect("write summary");
    }
    Fixture {
        _tempdir: tempdir,
        codex_home,
        global_root,
        local_root,
        other_root,
    }
}

fn backend(fixture: &Fixture) -> ScopedMemoriesBackend {
    ScopedMemoriesBackend {
        global: LocalMemoriesBackend::from_memory_root(fixture.global_root.clone()),
        local: LocalMemoriesBackend::from_memory_root(fixture.local_root.clone()),
    }
}

fn scope(may_write_global: bool) -> ScopedMemoriesConfig {
    ScopedMemoriesConfig {
        scope_key: Some("chat-1".to_string()),
        may_write_global,
        may_delete: false,
    }
}

fn read_request(path: &str) -> ReadMemoryRequest {
    ReadMemoryRequest {
        path: path.to_string(),
        line_offset: 1,
        max_lines: None,
        max_tokens: 1_000,
    }
}

#[tokio::test]
async fn scoped_backend_routes_global_and_local_paths() {
    let fixture = fixture().await;
    let backend = backend(&fixture);

    let root = backend
        .list(ListMemoriesRequest {
            path: None,
            cursor: None,
            max_results: 10,
        })
        .await
        .expect("list root");
    assert_eq!(
        root.entries
            .iter()
            .map(|entry| entry.path.as_str())
            .collect::<Vec<_>>(),
        vec!["global", "local"]
    );

    let local = backend
        .list(ListMemoriesRequest {
            path: Some("local".to_string()),
            cursor: None,
            max_results: 10,
        })
        .await
        .expect("list local");
    assert!(
        local
            .entries
            .iter()
            .any(|entry| entry.path == "local/MEMORY.md")
    );

    let global = backend
        .read(read_request("global/MEMORY.md"))
        .await
        .expect("read global");
    assert_eq!(global.path, "global/MEMORY.md");
    assert!(global.content.contains("shared fact"));
    let local = backend
        .read(read_request("local/MEMORY.md"))
        .await
        .expect("read local");
    assert!(local.content.contains("alice likes tea"));

    let search = backend
        .search(SearchMemoriesRequest {
            queries: vec!["e".to_string()],
            match_mode: SearchMatchMode::Any,
            path: None,
            cursor: None,
            context_lines: 0,
            case_sensitive: false,
            normalized: false,
            max_results: 50,
        })
        .await
        .expect("search both");
    let paths = search
        .matches
        .iter()
        .map(|found| found.path.as_str())
        .collect::<Vec<_>>();
    assert!(paths.iter().any(|path| path.starts_with("global/")));
    assert!(paths.iter().any(|path| path.starts_with("local/")));
    assert!(
        search
            .matches
            .iter()
            .all(|found| !found.content.contains("bob secret")),
        "other scopes must never be visible"
    );
}

#[tokio::test]
async fn scoped_backend_rejects_escapes_and_unknown_prefixes() {
    let fixture = fixture().await;
    let backend = backend(&fixture);
    for path in [
        "MEMORY.md",
        "memories_scopes/chat-2/MEMORY.md",
        "local/../../memories_scopes/chat-2/MEMORY.md",
        "global/../memories_scopes/chat-2/MEMORY.md",
        "local//etc/passwd",
        "global",
        "local",
        "/etc/passwd",
        "../memories_scopes/chat-2/MEMORY.md",
        "global/./../memories_scopes/chat-2/MEMORY.md",
        "Local/MEMORY.md",
        #[cfg(windows)]
        "local\\..\\..\\memories_scopes\\chat-2\\MEMORY.md",
        #[cfg(windows)]
        "global/..\\memories_scopes/chat-2/MEMORY.md",
        #[cfg(windows)]
        "global/C:\\Windows\\win.ini",
        #[cfg(windows)]
        "local/\\\\server\\share\\x.md",
        "C:\\Windows\\win.ini",
        "\\\\server\\share\\x.md",
    ] {
        let err = backend
            .read(read_request(path))
            .await
            .expect_err("path should be rejected");
        assert!(
            matches!(
                err,
                MemoriesBackendError::InvalidPath { .. } | MemoriesBackendError::NotFile { .. }
            ),
            "{path}: {err}"
        );
    }
}

#[tokio::test]
async fn scoped_prompt_merges_global_and_own_scope_only() {
    let fixture = fixture().await;
    let text = build_scoped_developer_instructions(
        &fixture.codex_home,
        MemoryVersion::V1,
        &scope(/*may_write_global*/ false),
        /*dedicated_tools*/ true,
    )
    .await
    .expect("instructions");
    assert!(text.contains("summary: shared fact"));
    assert!(text.contains("summary: alice likes tea"));
    assert!(!text.contains("bob secret"));
    assert!(text.contains("this chat cannot write shared memories"));

    let unscoped = build_scoped_developer_instructions(
        &fixture.codex_home,
        MemoryVersion::V1,
        &ScopedMemoriesConfig::default(),
        /*dedicated_tools*/ true,
    )
    .await;
    assert_eq!(unscoped, None);

    let private = build_scoped_developer_instructions(
        &fixture.codex_home,
        MemoryVersion::V1,
        &scope(/*may_write_global*/ true),
        /*dedicated_tools*/ false,
    )
    .await
    .expect("instructions");
    let local_notes = fixture
        .local_root
        .join("extensions")
        .join("ad_hoc")
        .join("notes");
    assert!(
        private.contains(&local_notes.display().to_string()),
        "filesystem notes must be redirected to the private folder"
    );
}

#[tokio::test]
async fn developer_instructions_are_upstream_without_fork_settings() {
    let fixture = fixture().await;
    let upstream = build_memory_tool_developer_instructions(&fixture.codex_home, MemoryVersion::V1)
        .await
        .expect("upstream instructions");
    for scope in [
        None,
        Some(ScopedMemoriesConfig {
            scope_key: None,
            may_write_global: true,
            may_delete: false,
        }),
    ] {
        assert_eq!(
            developer_instructions(
                &fixture.codex_home,
                MemoryVersion::V1,
                scope.as_ref(),
                /*dedicated_tools*/ true,
            )
            .await,
            Some(upstream.clone())
        );
    }
    let read_only = developer_instructions(
        &fixture.codex_home,
        MemoryVersion::V1,
        Some(&ScopedMemoriesConfig {
            scope_key: None,
            may_write_global: false,
            may_delete: false,
        }),
        /*dedicated_tools*/ true,
    )
    .await
    .expect("instructions");
    assert!(read_only.starts_with(&upstream));
    assert!(read_only.contains(NO_GLOBAL_WRITE_INSTRUCTIONS));
}

#[tokio::test]
async fn unscoped_threads_without_global_permission_get_no_ad_hoc_tool() {
    let fixture = fixture().await;
    let ad_hoc = ToolName::namespaced(MEMORY_TOOLS_NAMESPACE, ADD_AD_HOC_NOTE_TOOL_NAME);
    assert!(
        scoped_memory_tools(
            &fixture.codex_home,
            MemoryVersion::V1,
            &ScopedMemoriesConfig {
                scope_key: None,
                may_write_global: true,
                may_delete: false,
            },
            None,
        )
        .is_none(),
        "upstream tools are used without fork settings"
    );
    let tools = scoped_memory_tools(
        &fixture.codex_home,
        MemoryVersion::V1,
        &ScopedMemoriesConfig {
            scope_key: None,
            may_write_global: false,
            may_delete: false,
        },
        None,
    )
    .expect("read-only tools");
    assert_eq!(tools.len(), 3);
    assert!(tools.iter().all(|tool| tool.tool_name() != ad_hoc));
}

async fn call_ad_hoc(
    tools: &[Arc<dyn for<'call> ToolExecutor<ToolCall<'call>>>],
    arguments: serde_json::Value,
) -> Result<(), codex_extension_api::FunctionCallError> {
    let name = ToolName::namespaced(MEMORY_TOOLS_NAMESPACE, ADD_AD_HOC_NOTE_TOOL_NAME);
    let tool = tools
        .iter()
        .find(|tool| tool.tool_name() == name)
        .expect("ad hoc tool");
    tool.handle(ToolCall {
        turn_id: "turn-1".to_string(),
        call_id: "call-1".to_string(),
        tool_name: name,
        model: "gpt-test".to_string(),
        codex_turn_metadata: None,
        scopes: Vec::new(),
        truncation_policy: TruncationPolicy::Bytes(1024),
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
}

#[tokio::test]
async fn ad_hoc_notes_default_to_local_and_global_requires_permission() {
    let fixture = fixture().await;
    let notes = |root: &std::path::Path| root.join("extensions/ad_hoc/notes");

    let tools = scoped_memory_tools(
        &fixture.codex_home,
        MemoryVersion::V1,
        &scope(/*may_write_global*/ true),
        None,
    )
    .expect("tools");
    call_ad_hoc(
        &tools,
        json!({"filename": "2026-05-26T13-42-08-local-note.md", "note": "alice"}),
    )
    .await
    .expect("local note");
    call_ad_hoc(
        &tools,
        json!({"filename": "2026-05-26T13-42-09-global-note.md", "note": "tip", "scope": "global"}),
    )
    .await
    .expect("global note");
    assert!(
        notes(&fixture.local_root)
            .join("2026-05-26T13-42-08-local-note.md")
            .exists()
    );
    assert!(
        notes(&fixture.global_root)
            .join("2026-05-26T13-42-09-global-note.md")
            .exists()
    );

    // Without global write permission the upstream tool (no `scope`) writes locally.
    let tools = scoped_memory_tools(
        &fixture.codex_home,
        MemoryVersion::V1,
        &scope(/*may_write_global*/ false),
        None,
    )
    .expect("tools");
    assert!(
        call_ad_hoc(
            &tools,
            json!({"filename": "2026-05-26T13-42-10-denied.md", "note": "x", "scope": "global"}),
        )
        .await
        .is_err(),
        "scope argument must be rejected when the chat may not write globally"
    );
    call_ad_hoc(
        &tools,
        json!({"filename": "2026-05-26T13-42-11-private.md", "note": "x"}),
    )
    .await
    .expect("private note");
    assert!(
        notes(&fixture.local_root)
            .join("2026-05-26T13-42-11-private.md")
            .exists()
    );
    assert!(
        !notes(&fixture.global_root)
            .join("2026-05-26T13-42-10-denied.md")
            .exists()
    );
    assert!(!fixture.other_root.join("extensions").exists());
}
