//! Fork addition: the consolidation agent keeps a usable tool surface even
//! when the host locks its chat threads out of the filesystem.

use super::agent;
use codex_features::Feature;
use codex_model_provider::create_model_provider;
use codex_protocol::models::PermissionProfile;
use codex_protocol::openai_models::ToolMode;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::start_mock_server;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use tempfile::TempDir;

/// Reproduces an AstrBot-style host: Codex orchestrates, never touches the
/// machine, so chat threads run in code mode with no shell.
fn lock_down_like_a_chat_host(config: &mut codex_core::config::Config) {
    config.model_tool_mode = Some(ToolMode::CodeModeOnly);
    config.include_environment_context = false;
    let _ = config.features.disable(Feature::ShellTool);
}

#[tokio::test]
async fn consolidation_regains_file_editing_tools_in_fork_mode() -> anyhow::Result<()> {
    let server = start_mock_server().await;
    let home = Arc::new(TempDir::new()?);
    let test = test_codex()
        .with_home(home)
        .build_with_auto_env(&server)
        .await?;
    let provider = create_model_provider(
        test.config.model_provider.clone(),
        Some(test.thread_manager.auth_manager()),
    );

    let mut parent = test.config.clone();
    lock_down_like_a_chat_host(&mut parent);
    // A scope key is what puts the fork's memory routing in play.
    parent.memories.scope_key = Some("aiocqhttp:GroupMessage:12345".to_string());
    // With an executable there is a local execution environment to use.
    parent.codex_self_exe = Some(std::path::PathBuf::from("codex"));

    let mut agent_config =
        agent::get_config(&parent, PermissionProfile::Disabled, provider.as_ref())
            .expect("agent config should be created");
    crate::scopes::adjust_agent_config(&mut agent_config, &parent);

    assert_eq!(
        agent_config.model_tool_mode, None,
        "consolidation must call tools directly, not through the code-mode host"
    );
    assert!(
        agent_config.features.enabled(Feature::ShellTool),
        "consolidation rewrites files under the memory root and needs a shell"
    );
    assert!(
        agent_config.include_environment_context,
        "with a shell back the agent needs to know which platform it runs on"
    );
    // The host persona must still not leak into the consolidation prompt.
    assert_eq!(agent_config.base_instructions, None);

    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test]
async fn upstream_threads_keep_the_inherited_tool_surface() -> anyhow::Result<()> {
    let server = start_mock_server().await;
    let home = Arc::new(TempDir::new()?);
    let test = test_codex()
        .with_home(home)
        .build_with_auto_env(&server)
        .await?;
    let provider = create_model_provider(
        test.config.model_provider.clone(),
        Some(test.thread_manager.auth_manager()),
    );

    // Upstream defaults: no scope, may write globally, auto consolidate.
    let mut parent = test.config.clone();
    parent.memories.may_write_global = true;
    parent.memories.auto_consolidate = true;
    parent.memories.extra_session_sources = Vec::new();
    parent.memories.scope_key = None;
    lock_down_like_a_chat_host(&mut parent);

    let mut agent_config =
        agent::get_config(&parent, PermissionProfile::Disabled, provider.as_ref())
            .expect("agent config should be created");
    crate::scopes::adjust_agent_config(&mut agent_config, &parent);

    assert_eq!(agent_config.model_tool_mode, Some(ToolMode::CodeModeOnly));
    assert!(!agent_config.features.enabled(Feature::ShellTool));

    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test]
async fn without_a_codex_executable_consolidation_gets_memory_file_tools() -> anyhow::Result<()> {
    let server = start_mock_server().await;
    let home = Arc::new(TempDir::new()?);
    let test = test_codex()
        .with_home(home)
        .build_with_auto_env(&server)
        .await?;
    let provider = create_model_provider(
        test.config.model_provider.clone(),
        Some(test.thread_manager.auth_manager()),
    );

    let mut parent = test.config.clone();
    lock_down_like_a_chat_host(&mut parent);
    parent.memories.scope_key = Some("aiocqhttp:GroupMessage:12345".to_string());
    // No executable means no local execution environment at all.
    parent.codex_self_exe = None;

    let mut agent_config =
        agent::get_config(&parent, PermissionProfile::Disabled, provider.as_ref())
            .expect("agent config should be created");
    crate::scopes::adjust_agent_config(&mut agent_config, &parent);

    assert!(
        agent_config.memories.maintenance_tools,
        "the memories extension must serve the files instead"
    );
    assert!(
        !agent_config.features.enabled(Feature::ShellTool),
        "a shell tool is never registered without an environment; enabling it would only mislead"
    );
    assert_eq!(agent_config.model_tool_mode, None);

    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test]
async fn a_codex_executable_keeps_the_shell_and_no_file_tools() -> anyhow::Result<()> {
    let server = start_mock_server().await;
    let home = Arc::new(TempDir::new()?);
    let test = test_codex()
        .with_home(home)
        .build_with_auto_env(&server)
        .await?;
    let provider = create_model_provider(
        test.config.model_provider.clone(),
        Some(test.thread_manager.auth_manager()),
    );

    let mut parent = test.config.clone();
    lock_down_like_a_chat_host(&mut parent);
    parent.memories.scope_key = Some("aiocqhttp:GroupMessage:12345".to_string());
    parent.codex_self_exe = Some(std::path::PathBuf::from("codex"));

    let mut agent_config =
        agent::get_config(&parent, PermissionProfile::Disabled, provider.as_ref())
            .expect("agent config should be created");
    crate::scopes::adjust_agent_config(&mut agent_config, &parent);

    assert!(agent_config.features.enabled(Feature::ShellTool));
    assert!(!agent_config.memories.maintenance_tools);

    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test]
async fn the_maintenance_note_is_appended_only_when_the_tools_are_on() -> anyhow::Result<()> {
    let server = start_mock_server().await;
    let home = Arc::new(TempDir::new()?);
    let test = test_codex()
        .with_home(home)
        .build_with_auto_env(&server)
        .await?;

    let prompt = || {
        vec![UserInput::Text {
            text: "Consolidate the supplied rollout summaries".to_string(),
            text_elements: vec![],
        }]
    };
    let text = |prompt: &[UserInput]| match &prompt[0] {
        UserInput::Text { text, .. } => text.clone(),
        _ => unreachable!(),
    };

    let mut config = test.config.clone();
    let mut off = prompt();
    crate::scopes::append_maintenance_tools_note(&config, &mut off);
    assert!(!text(&off).contains("memories.write"));

    config.memories.maintenance_tools = true;
    let mut on = prompt();
    crate::scopes::append_maintenance_tools_note(&config, &mut on);
    let rendered = text(&on);
    assert!(rendered.contains("memories.list"));
    assert!(rendered.contains("memories.read"));
    assert!(rendered.contains("memories.write"));
    assert!(rendered.contains("no shell"));

    test.codex.shutdown_and_wait().await?;
    Ok(())
}
