//! Fork addition: startup tests for per-chat memory scopes.

use super::*;
use crate::scopes::Phase2Target;
use codex_config::memory_scopes::memory_scope_root;
use pretty_assertions::assert_eq;

fn scoped_memories_config(auto_consolidate: bool) -> MemoriesConfig {
    MemoriesConfig {
        scope_key: Some("chat-1".to_string()),
        may_write_global: false,
        auto_consolidate,
        ..startup_test_memories_config()
    }
}

#[tokio::test]
async fn auto_consolidate_false_registers_scope_but_skips_phases() -> anyhow::Result<()> {
    let server = start_mock_server().await;
    let home = Arc::new(TempDir::new()?);
    let db = init_state_db(&home).await?;
    seed_stage1_output(
        db.as_ref(),
        home.path(),
        chrono::Utc::now(),
        "raw memory",
        "rollout summary",
        "manual-only",
    )
    .await?;
    let test = build_test_codex_with_memories_config(
        &server,
        home.clone(),
        scoped_memories_config(/*auto_consolidate*/ false),
    )
    .await?;
    let scope_root = memory_scope_root(&test.config.codex_home, "memories", "chat-1");

    trigger_memories_startup(&test).await;
    wait_for_dir(&scope_root.join("extensions").join("ad_hoc")).await?;
    let thread_id = test.session_configured.thread_id;
    let deadline = Instant::now() + Duration::from_secs(10);
    let scope = loop {
        if let Some(scope) = db.memories().thread_memory_scope(thread_id).await? {
            break scope;
        }
        anyhow::ensure!(Instant::now() < deadline, "scope was not registered");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(
        scope,
        codex_state::ThreadMemoryScope {
            scope_key: Some("chat-1".to_string()),
            may_write_global: false,
        }
    );

    tokio::time::sleep(Duration::from_millis(500)).await;
    let requests = server.received_requests().await.unwrap_or_default();
    assert!(
        requests
            .iter()
            .all(|request| !request.url.path().ends_with("/responses")),
        "auto_consolidate=false must not sample phase 1 or phase 2"
    );
    // Nobody claimed the global phase-2 job.
    assert!(matches!(
        db.memories()
            .try_claim_global_phase2_job(ThreadId::new(), /*lease_seconds*/ 3_600)
            .await?,
        Phase2JobClaimOutcome::Claimed { .. }
    ));

    shutdown_test_codex(&test).await?;
    Ok(())
}

#[tokio::test]
async fn scoped_phase2_consolidates_only_the_scope_partition() -> anyhow::Result<()> {
    let server = start_mock_server().await;
    let home = Arc::new(TempDir::new()?);
    let test = build_test_codex_with_memories_config(
        &server,
        home.clone(),
        scoped_memories_config(/*auto_consolidate*/ false),
    )
    .await?;
    let db = test
        .codex
        .state_db()
        .ok_or_else(|| anyhow::anyhow!("state db should be enabled"))?;

    // One global output and one private output of chat-1.
    seed_stage1_output(
        db.as_ref(),
        home.path(),
        chrono::Utc::now(),
        "global raw",
        "global summary",
        "global-slug",
    )
    .await?;
    let private_thread =
        seed_stage1_candidate(db.as_ref(), home.path(), chrono::Utc::now(), "private-slug").await?;
    let updated_at = db
        .get_thread(private_thread)
        .await?
        .expect("thread")
        .updated_at
        .timestamp();
    let codex_state::Stage1JobClaimOutcome::Claimed { ownership_token } = db
        .memories()
        .try_claim_stage1_job(private_thread, ThreadId::new(), updated_at, 3_600, 64)
        .await?
    else {
        panic!("claim private thread");
    };
    assert!(
        db.memories()
            .mark_stage1_job_succeeded_in_partition(
                private_thread,
                &ownership_token,
                updated_at,
                "private raw",
                "private summary",
                Some("private-slug"),
                "scope:chat-1",
            )
            .await?
    );

    let response = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-scope"),
            ev_assistant_message("msg-scope", "phase2 complete"),
            ev_completed("resp-scope"),
        ]),
    )
    .await;
    let provider = Arc::new(MockMemoryModelProvider::new(
        test.config.model_provider.clone(),
        Some(test.thread_manager.auth_manager()),
    ));
    let (context, config) = memory_startup_context_with_provider(&test, provider).await;
    let target = Phase2Target::scope(&config, "chat-1", /*skip_cooldown*/ false);
    let root = target.root.clone();
    tokio::fs::create_dir_all(&root).await?;
    seed_extension_instructions(&root).await?;
    seed_required_memory_artifacts(&root).await?;
    let parent_permission_profile = config.permissions.effective_permission_profile();
    phase2::run_for_target(context, config, parent_permission_profile, target).await;

    let request = wait_for_single_request(&response).await;
    assert!(request.body_contains_text("Private chat memory store"));
    let summaries = read_rollout_summary_bodies(&root).await?;
    assert_eq!(summaries.len(), 1);
    assert!(summaries[0].contains("private summary"));
    assert!(
        !home
            .path()
            .join("memories")
            .join("rollout_summaries")
            .exists(),
        "the global root must not receive scoped inputs"
    );

    shutdown_test_codex(&test).await?;
    Ok(())
}
