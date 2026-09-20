//! Fork addition: the consolidation agent keeps a usable tool surface even
//! when the host locks its chat threads out of the filesystem.

use super::agent;
use codex_features::Feature;
use codex_model_provider::create_model_provider;
use codex_protocol::models::PermissionProfile;
use codex_protocol::openai_models::ToolMode;
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
