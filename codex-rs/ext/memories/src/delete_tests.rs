//! Fork addition: tests for entry-level memory deletion (`memories.may_delete`).

use std::path::Path;
use std::sync::Arc;

use codex_extension_api::ConversationHistory;
use codex_extension_api::NoopTurnItemEmitter;
use codex_extension_api::ToolCall;
use codex_extension_api::ToolCallSource;
use codex_extension_api::ToolExecutor;
use codex_extension_api::ToolName;
use codex_extension_api::ToolPayload;
use codex_protocol::MemoryVersion;
use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_absolute_path::test_support::PathExt;
use codex_utils_output_truncation::TruncationPolicy;
use pretty_assertions::assert_eq;
use serde_json::json;

use crate::ADD_AD_HOC_NOTE_TOOL_NAME;
use crate::DELETE_TOOL_NAME;
use crate::MEMORY_TOOLS_NAMESPACE;
use crate::backend::AddAdHocMemoryNoteRequest;
use crate::backend::DeleteMemoryRequest;
use crate::backend::DeleteMemoryResponse;
use crate::backend::MemoriesBackend;
use crate::backend::MemoriesBackendError;
use crate::local::LocalMemoriesBackend;
use crate::prompts::build_memory_tool_developer_instructions;
use crate::scoped::ScopedMemoriesConfig;
use crate::scoped::build_scoped_developer_instructions;
use crate::scoped::developer_instructions;
use crate::scoped::scope_root;
use crate::scoped::scoped_memory_tools;
use crate::tools::DeleteMemoryTool;

const NOTE: &str = "2026-05-26T13-42-08-remember-this.md";
const OTHER_NOTE: &str = "2026-05-26T13-42-09-and-this.md";

type MemoryTool = Arc<dyn for<'call> ToolExecutor<ToolCall<'call>>>;

struct Fixture {
    _tempdir: tempfile::TempDir,
    codex_home: AbsolutePathBuf,
    global_root: std::path::PathBuf,
    local_root: std::path::PathBuf,
}

async fn fixture() -> Fixture {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let codex_home = tempdir.path().abs();
    let global_root = tempdir.path().join("memories");
    let local_root = scope_root(&codex_home, MemoryVersion::V1, "chat-1").to_path_buf();
    for (root, text) in [
        (&global_root, "shared fact"),
        (&local_root, "alice likes tea"),
    ] {
        tokio::fs::create_dir_all(root).await.expect("mkdir");
        tokio::fs::write(root.join("MEMORY.md"), format!("{text}\n"))
            .await
            .expect("write memory");
        tokio::fs::write(root.join("memory_summary.md"), format!("summary: {text}"))
            .await
            .expect("write summary");
        let backend = LocalMemoriesBackend::from_memory_root(root.clone());
        for filename in [NOTE, OTHER_NOTE] {
            backend
                .add_ad_hoc_note(AddAdHocMemoryNoteRequest {
                    filename: filename.to_string(),
                    note: format!("{text} note\n"),
                })
                .await
                .expect("seed ad-hoc note");
        }
    }
    Fixture {
        _tempdir: tempdir,
        codex_home,
        global_root,
        local_root,
    }
}

fn note_path(root: &Path, filename: &str) -> std::path::PathBuf {
    root.join("extensions")
        .join("ad_hoc")
        .join("notes")
        .join(filename)
}

fn note_relative_path(filename: &str) -> String {
    format!("extensions/ad_hoc/notes/{filename}")
}

fn scope(may_write_global: bool, may_delete: bool) -> ScopedMemoriesConfig {
    ScopedMemoriesConfig {
        turn_scopes: false,
        scope_key: Some("chat-1".to_string()),
        may_write_global,
        may_delete,
    }
}

fn delete_tool_name() -> ToolName {
    ToolName::namespaced(MEMORY_TOOLS_NAMESPACE, DELETE_TOOL_NAME)
}

fn has_delete_tool(tools: &[MemoryTool]) -> bool {
    tools
        .iter()
        .any(|tool| tool.tool_name() == delete_tool_name())
}

async fn call_delete(tools: &[MemoryTool], path: &str) -> Result<(), String> {
    let name = delete_tool_name();
    let tool = tools
        .iter()
        .find(|tool| tool.tool_name() == name)
        .expect("delete tool");
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
            arguments: json!({ "path": path }).to_string(),
        },
    })
    .await
    .map(|_| ())
    .map_err(|err| err.to_string())
}

fn create_file_symlink(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        // Unprivileged Windows accounts cannot create symlinks; skip there.
        std::os::windows::fs::symlink_file(target, link).is_ok()
    }
}

#[tokio::test]
async fn local_backend_deletes_one_memory_file() {
    let fixture = fixture().await;
    let backend = LocalMemoriesBackend::from_memory_root(fixture.global_root.clone());

    let response = backend
        .delete(DeleteMemoryRequest {
            path: note_relative_path(NOTE),
        })
        .await
        .expect("delete note");
    assert_eq!(
        response,
        DeleteMemoryResponse {
            path: note_relative_path(NOTE),
            deleted: true,
        }
    );
    assert!(!note_path(&fixture.global_root, NOTE).exists());
    assert!(
        note_path(&fixture.global_root, OTHER_NOTE).exists(),
        "only the requested memory is removed"
    );
    assert!(fixture.global_root.join("MEMORY.md").exists());

    // Deleting it again reports a missing file instead of panicking.
    let err = backend
        .delete(DeleteMemoryRequest {
            path: note_relative_path(NOTE),
        })
        .await
        .expect_err("second delete fails");
    assert!(
        matches!(err, MemoriesBackendError::NotFound { .. }),
        "{err}"
    );
}

#[tokio::test]
async fn local_backend_refuses_directories() {
    let fixture = fixture().await;
    let backend = LocalMemoriesBackend::from_memory_root(fixture.global_root.clone());

    for path in ["extensions", "extensions/ad_hoc/notes", "", "."] {
        let err = backend
            .delete(DeleteMemoryRequest {
                path: path.to_string(),
            })
            .await
            .expect_err("directories cannot be deleted");
        assert!(
            matches!(
                err,
                MemoriesBackendError::NotFile { .. } | MemoriesBackendError::NotFound { .. }
            ),
            "{path}: {err}"
        );
    }
    assert!(note_path(&fixture.global_root, NOTE).exists());
    assert!(fixture.global_root.join("extensions").is_dir());
}

#[tokio::test]
async fn local_backend_rejects_escaping_and_hidden_paths() {
    let fixture = fixture().await;
    let outside = fixture
        .global_root
        .parent()
        .expect("parent")
        .join("outside.md");
    tokio::fs::write(&outside, "outside\n")
        .await
        .expect("write outside file");
    tokio::fs::create_dir_all(fixture.global_root.join(".hidden"))
        .await
        .expect("mkdir hidden");
    tokio::fs::write(fixture.global_root.join(".hidden/secret.md"), "secret\n")
        .await
        .expect("write hidden file");
    let backend = LocalMemoriesBackend::from_memory_root(fixture.global_root.clone());

    let mut paths = vec![
        "../outside.md".to_string(),
        "extensions/../../outside.md".to_string(),
        "./../outside.md".to_string(),
        "/etc/passwd".to_string(),
        ".hidden/secret.md".to_string(),
        fixture.global_root.join("MEMORY.md").display().to_string(),
        outside.display().to_string(),
    ];
    if cfg!(windows) {
        paths.push("..\\outside.md".to_string());
        paths.push("C:\\Windows\\win.ini".to_string());
        paths.push("\\\\server\\share\\x.md".to_string());
    }
    for path in paths {
        let err = backend
            .delete(DeleteMemoryRequest { path: path.clone() })
            .await
            .expect_err("path should be rejected");
        assert!(
            matches!(
                err,
                MemoriesBackendError::InvalidPath { .. } | MemoriesBackendError::NotFound { .. }
            ),
            "{path}: {err}"
        );
    }
    assert!(outside.exists(), "paths outside the root must survive");
    assert!(fixture.global_root.join(".hidden/secret.md").exists());
    assert!(fixture.global_root.join("MEMORY.md").exists());
}

#[tokio::test]
async fn local_backend_refuses_symlinked_memories() {
    let fixture = fixture().await;
    let outside = fixture
        .global_root
        .parent()
        .expect("parent")
        .join("outside.md");
    tokio::fs::write(&outside, "outside\n")
        .await
        .expect("write outside file");
    let link = fixture.global_root.join("linked.md");
    if !create_file_symlink(&outside, &link) {
        // Unprivileged Windows accounts cannot create symlinks.
        return;
    }

    let backend = LocalMemoriesBackend::from_memory_root(fixture.global_root.clone());
    let err = backend
        .delete(DeleteMemoryRequest {
            path: "linked.md".to_string(),
        })
        .await
        .expect_err("symlinks must be refused");
    assert!(
        matches!(err, MemoriesBackendError::InvalidPath { .. }),
        "{err}"
    );
    assert!(link.exists(), "the symlink itself is left alone");
    assert!(outside.exists(), "the symlink target is left alone");
}

#[tokio::test]
async fn delete_tool_is_absent_unless_may_delete_is_set() {
    let fixture = fixture().await;
    for scope in [
        scope(/*may_write_global*/ true, /*may_delete*/ false),
        scope(/*may_write_global*/ false, /*may_delete*/ false),
    ] {
        let tools = scoped_memory_tools(&fixture.codex_home, MemoryVersion::V1, &scope, None)
            .expect("scoped tools");
        assert_eq!(tools.len(), 4);
        assert!(!has_delete_tool(&tools));
    }

    // An unscoped thread keeps the upstream tool set when the switch is off.
    assert!(
        scoped_memory_tools(
            &fixture.codex_home,
            MemoryVersion::V1,
            &ScopedMemoriesConfig {
                turn_scopes: false,
                scope_key: None,
                may_write_global: true,
                may_delete: false,
            },
            None,
        )
        .is_none()
    );

    // A read-only unscoped thread never gets the tool, even with the switch on.
    let read_only = scoped_memory_tools(
        &fixture.codex_home,
        MemoryVersion::V1,
        &ScopedMemoriesConfig {
            turn_scopes: false,
            scope_key: None,
            may_write_global: false,
            may_delete: true,
        },
        None,
    )
    .expect("read-only tools");
    assert!(!has_delete_tool(&read_only));

    for scope in [
        scope(/*may_write_global*/ true, /*may_delete*/ true),
        scope(/*may_write_global*/ false, /*may_delete*/ true),
        ScopedMemoriesConfig {
            turn_scopes: false,
            scope_key: None,
            may_write_global: true,
            may_delete: true,
        },
    ] {
        let tools = scoped_memory_tools(&fixture.codex_home, MemoryVersion::V1, &scope, None)
            .expect("tools with delete");
        assert_eq!(tools.len(), 5);
        assert!(has_delete_tool(&tools));
    }
}

#[tokio::test]
async fn delete_tool_spec_matches_expected_schema() {
    let fixture = fixture().await;
    let tools = scoped_memory_tools(
        &fixture.codex_home,
        MemoryVersion::V1,
        &ScopedMemoriesConfig {
            turn_scopes: false,
            scope_key: None,
            may_write_global: true,
            may_delete: true,
        },
        None,
    )
    .expect("tools with delete");
    let tool = tools
        .iter()
        .find(|tool| tool.tool_name() == delete_tool_name())
        .expect("delete tool");
    assert_eq!(
        serde_json::to_value(tool.spec()).expect("serialize spec")["tools"][0],
        json!({
            "type": "function",
            "name": "delete_memory",
            "description": "Permanently delete one Codex memory file by relative path. Only use it when the user explicitly asks to delete or forget a stored memory, and delete nothing else.",
            "strict": false,
            "parameters": {
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Relative path of the memory file to delete, as returned by the list, search, and read tools.",
                    },
                },
                "required": ["path"],
                "additionalProperties": false,
            },
        })
    );
}

#[tokio::test]
async fn scoped_delete_tool_removes_local_notes_and_guards_the_shared_store() {
    let fixture = fixture().await;

    // A chat that may not write globally can still curate its own memories.
    let tools = scoped_memory_tools(
        &fixture.codex_home,
        MemoryVersion::V1,
        &scope(/*may_write_global*/ false, /*may_delete*/ true),
        None,
    )
    .expect("tools");
    call_delete(&tools, &format!("local/{}", note_relative_path(NOTE)))
        .await
        .expect("delete own note");
    assert!(!note_path(&fixture.local_root, NOTE).exists());

    let err = call_delete(&tools, &format!("global/{}", note_relative_path(NOTE)))
        .await
        .expect_err("shared memories are protected");
    assert!(err.contains("may only delete its own"), "{err}");
    assert!(
        note_path(&fixture.global_root, NOTE).exists(),
        "shared note must survive"
    );
    assert!(
        call_delete(&tools, "global/MEMORY.md").await.is_err(),
        "shared memories are protected"
    );
    assert!(fixture.global_root.join("MEMORY.md").exists());

    // With permission to write globally the shared store can be curated too.
    let tools = scoped_memory_tools(
        &fixture.codex_home,
        MemoryVersion::V1,
        &scope(/*may_write_global*/ true, /*may_delete*/ true),
        None,
    )
    .expect("tools");
    call_delete(&tools, &format!("global/{}", note_relative_path(NOTE)))
        .await
        .expect("delete shared note");
    assert!(!note_path(&fixture.global_root, NOTE).exists());
    assert!(note_path(&fixture.global_root, OTHER_NOTE).exists());

    // Paths must still use the scope prefixes and stay inside the roots.
    for path in [
        "MEMORY.md",
        "local/../../memories_scopes/chat-2/MEMORY.md",
        "local",
        "/etc/passwd",
    ] {
        assert!(
            call_delete(&tools, path).await.is_err(),
            "{path} must be rejected"
        );
    }
    assert!(fixture.local_root.join("MEMORY.md").exists());
}

#[tokio::test]
async fn unscoped_delete_tool_uses_the_single_root() {
    let fixture = fixture().await;
    let tools = scoped_memory_tools(
        &fixture.codex_home,
        MemoryVersion::V1,
        &ScopedMemoriesConfig {
            turn_scopes: false,
            scope_key: None,
            may_write_global: true,
            may_delete: true,
        },
        None,
    )
    .expect("tools");
    call_delete(&tools, &note_relative_path(NOTE))
        .await
        .expect("delete note");
    assert!(!note_path(&fixture.global_root, NOTE).exists());
    assert!(call_delete(&tools, "../outside.md").await.is_err());
}

#[tokio::test]
async fn delete_tool_without_scope_prefix_support_ignores_global_guard() {
    let fixture = fixture().await;
    // The unscoped tool addresses the single root directly, so a literal
    // `global/...` directory inside that root is not special-cased.
    tokio::fs::create_dir_all(fixture.global_root.join("global"))
        .await
        .expect("mkdir");
    tokio::fs::write(fixture.global_root.join("global/note.md"), "x\n")
        .await
        .expect("write");
    let tool = DeleteMemoryTool {
        permission: crate::permission::Permission::Fixed(true),
        global_permission: crate::permission::Permission::Fixed(true),
        backend: LocalMemoriesBackend::from_memory_root(fixture.global_root.clone()),
        may_delete_global: true,
        metrics_client: None,
    };
    let tools: Vec<MemoryTool> = vec![Arc::new(tool)];
    call_delete(&tools, "global/note.md")
        .await
        .expect("delete note");
    assert!(!fixture.global_root.join("global/note.md").exists());
}

#[tokio::test]
async fn prompts_mention_the_tool_only_when_it_is_registered() {
    let fixture = fixture().await;
    let upstream = build_memory_tool_developer_instructions(&fixture.codex_home, MemoryVersion::V1)
        .await
        .expect("upstream instructions");

    // Switch off: byte-identical to upstream.
    for scope in [
        ScopedMemoriesConfig {
            turn_scopes: false,
            scope_key: None,
            may_write_global: true,
            may_delete: false,
        },
        ScopedMemoriesConfig::default(),
    ] {
        let text = developer_instructions(
            &fixture.codex_home,
            MemoryVersion::V1,
            Some(&scope),
            /*dedicated_tools*/ true,
        )
        .await;
        if scope.may_write_global {
            assert_eq!(text, Some(upstream.clone()));
        } else {
            assert!(!text.expect("instructions").contains(DELETE_TOOL_NAME));
        }
    }

    // Switch on, but no dedicated tools: nothing is registered, nothing is said.
    let no_tools = developer_instructions(
        &fixture.codex_home,
        MemoryVersion::V1,
        Some(&ScopedMemoriesConfig {
            turn_scopes: false,
            scope_key: None,
            may_write_global: true,
            may_delete: true,
        }),
        /*dedicated_tools*/ false,
    )
    .await;
    assert_eq!(no_tools, Some(upstream.clone()));

    let with_delete = developer_instructions(
        &fixture.codex_home,
        MemoryVersion::V1,
        Some(&ScopedMemoriesConfig {
            turn_scopes: false,
            scope_key: None,
            may_write_global: true,
            may_delete: true,
        }),
        /*dedicated_tools*/ true,
    )
    .await
    .expect("instructions");
    assert!(with_delete.starts_with(&upstream));
    assert!(with_delete.contains(DELETE_TOOL_NAME));

    // Scoped prompts.
    let scoped_off = build_scoped_developer_instructions(
        &fixture.codex_home,
        MemoryVersion::V1,
        &scope(/*may_write_global*/ true, /*may_delete*/ false),
        /*dedicated_tools*/ true,
    )
    .await
    .expect("instructions");
    assert!(!scoped_off.contains(DELETE_TOOL_NAME));

    let scoped_on = build_scoped_developer_instructions(
        &fixture.codex_home,
        MemoryVersion::V1,
        &scope(/*may_write_global*/ true, /*may_delete*/ true),
        /*dedicated_tools*/ true,
    )
    .await
    .expect("instructions");
    assert!(scoped_on.contains(DELETE_TOOL_NAME));
    assert!(scoped_on.contains("`global/` or `local/` path"));
    assert_eq!(
        scoped_on.replace(
            "- `delete_memory` permanently deletes one memory file at the given `global/` or `local/` path; use it only when the user asks to delete or forget a stored memory.
",
            "",
        ),
        scoped_off,
        "the delete bullet is the only difference"
    );

    let scoped_local_only = build_scoped_developer_instructions(
        &fixture.codex_home,
        MemoryVersion::V1,
        &scope(/*may_write_global*/ false, /*may_delete*/ true),
        /*dedicated_tools*/ true,
    )
    .await
    .expect("instructions");
    assert!(scoped_local_only.contains("shared memories cannot be deleted here"));
}

#[tokio::test]
async fn ad_hoc_notes_can_be_added_and_deleted_again() {
    let fixture = fixture().await;
    let tools = scoped_memory_tools(
        &fixture.codex_home,
        MemoryVersion::V1,
        &scope(/*may_write_global*/ false, /*may_delete*/ true),
        None,
    )
    .expect("tools");
    let add = tools
        .iter()
        .find(|tool| {
            tool.tool_name()
                == ToolName::namespaced(MEMORY_TOOLS_NAMESPACE, ADD_AD_HOC_NOTE_TOOL_NAME)
        })
        .expect("ad hoc tool");
    let filename = "2026-05-26T13-42-30-temporary.md";
    add.handle(ToolCall {
        turn_id: "turn-1".to_string(),
        call_id: "call-1".to_string(),
        tool_name: ToolName::namespaced(MEMORY_TOOLS_NAMESPACE, ADD_AD_HOC_NOTE_TOOL_NAME),
        model: "gpt-test".to_string(),
        codex_turn_metadata: None,
        scopes: Vec::new(),
        truncation_policy: TruncationPolicy::Bytes(1024),
        source: ToolCallSource::Direct,
        conversation_history: ConversationHistory::default(),
        turn_item_emitter: Arc::new(NoopTurnItemEmitter),
        environments: Vec::new(),
        payload: ToolPayload::Function {
            arguments: json!({ "filename": filename, "note": "forget me" }).to_string(),
        },
    })
    .await
    .expect("add note");
    assert!(note_path(&fixture.local_root, filename).exists());

    call_delete(&tools, &format!("local/{}", note_relative_path(filename)))
        .await
        .expect("delete note");
    assert!(!note_path(&fixture.local_root, filename).exists());
}

async fn call_with_scopes(
    tools: &[MemoryTool],
    tool: &str,
    arguments: serde_json::Value,
    scopes: &[&str],
) -> Result<(), String> {
    let name = ToolName::namespaced(MEMORY_TOOLS_NAMESPACE, tool);
    let tool = tools
        .iter()
        .find(|candidate| candidate.tool_name() == name)
        .expect("memory tool");
    tool.handle(ToolCall {
        turn_id: "turn-1".to_string(),
        call_id: "call-1".to_string(),
        tool_name: name,
        model: "gpt-test".to_string(),
        codex_turn_metadata: None,
        scopes: scopes.iter().map(ToString::to_string).collect(),
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
    .map_err(|err| err.to_string())
}

async fn note(tools: &[MemoryTool], filename: &str, scopes: &[&str]) -> Result<(), String> {
    let arguments = json!({ "filename": filename, "note": "fact
", "scope": "global" });
    call_with_scopes(tools, ADD_AD_HOC_NOTE_TOOL_NAME, arguments, scopes).await
}

async fn delete(tools: &[MemoryTool], path: &str, scopes: &[&str]) -> Result<(), String> {
    call_with_scopes(tools, DELETE_TOOL_NAME, json!({ "path": path }), scopes).await
}

#[tokio::test]
async fn turn_scopes_decide_what_the_current_sender_may_curate() {
    let fixture = fixture().await;
    let config = ScopedMemoriesConfig {
        scope_key: Some("chat-1".to_string()),
        // Consolidation stays private; each turn's scopes decide the rest.
        may_write_global: false,
        may_delete: false,
        turn_scopes: true,
    };
    let tools =
        scoped_memory_tools(&fixture.codex_home, MemoryVersion::V1, &config, None).expect("tools");
    // Registered for everyone: the tool set is the same whoever speaks.
    assert!(has_delete_tool(&tools));

    // A turn without scopes: private notes only, nothing deleted.
    let local = json!({
        "filename": "2026-05-26T13-50-00-private.md",
        "note": "fact
    ",
        "scope": "local",
    });
    call_with_scopes(&tools, ADD_AD_HOC_NOTE_TOOL_NAME, local, &[])
        .await
        .expect("private note");
    let refused = note(&tools, "2026-05-26T13-50-01-no.md", &[])
        .await
        .expect_err("shared note refused");
    assert!(
        refused.contains("may not write shared memories"),
        "{refused}"
    );
    assert!(!note_path(&fixture.global_root, "2026-05-26T13-50-01-no.md").exists());
    let local_note = format!("local/{}", note_relative_path(NOTE));
    assert!(delete(&tools, &local_note, &[]).await.is_err());
    assert!(note_path(&fixture.local_root, NOTE).exists());

    // Deleting needs its own scope; shared memories also need write_global.
    let global_note = format!("global/{}", note_relative_path(NOTE));
    assert!(
        delete(&tools, &global_note, &["memory.delete"])
            .await
            .is_err()
    );
    assert!(note_path(&fixture.global_root, NOTE).exists());
    delete(&tools, &local_note, &["memory.delete"])
        .await
        .expect("delete private");
    assert!(!note_path(&fixture.local_root, NOTE).exists());

    // A sender granted both, in a group or not, curates the shared store.
    let all = ["memory.write_global", "memory.delete"];
    note(&tools, "2026-05-26T13-50-02-yes.md", &all)
        .await
        .expect("shared note");
    assert!(note_path(&fixture.global_root, "2026-05-26T13-50-02-yes.md").exists());
    delete(&tools, &global_note, &all)
        .await
        .expect("delete shared");
    assert!(!note_path(&fixture.global_root, NOTE).exists());

    let text = build_scoped_developer_instructions(
        &fixture.codex_home,
        MemoryVersion::V1,
        &config,
        /*dedicated_tools*/ true,
    )
    .await
    .expect("instructions");
    assert!(text.contains("message metadata says they may manage shared memories"));
    assert!(!text.contains("may not write to the shared folder at all"));
}
