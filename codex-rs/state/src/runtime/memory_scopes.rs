//! Fork addition: per-chat memory scopes.
//!
//! Threads configured with `memories.scope_key` (or with
//! `memories.may_write_global = false`) register a `thread_memory_scope` row.
//! Stage-1 outputs of such threads are stored in a memory partition
//! (`global` or `scope:<key>`) and each partition is consolidated by its own
//! Phase 2 job (`job_key = partition`). Threads without a scope row keep the
//! upstream behavior: their outputs live in the `global` partition.

use super::*;

/// Memory scope registered for one thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadMemoryScope {
    /// Private scope of the thread, if any. The first registered key sticks.
    pub scope_key: Option<String>,
    /// Whether the thread may contribute to the global store. Once a thread
    /// was registered with `false` it stays `false`.
    pub may_write_global: bool,
}

impl MemoryStore {
    /// Registers (or updates) the memory scope of a thread.
    ///
    /// Both fields are sticky: the first non-null `scope_key` is kept (a
    /// thread's rollout never moves into another chat's scope), and
    /// `may_write_global` is AND-accumulated (once `false`, it stays `false`).
    pub async fn upsert_thread_memory_scope(
        &self,
        thread_id: ThreadId,
        scope_key: Option<&str>,
        may_write_global: bool,
    ) -> anyhow::Result<()> {
        sqlx::query(
            r#"
INSERT INTO thread_memory_scope (thread_id, scope_key, may_write_global)
VALUES (?, ?, ?)
ON CONFLICT(thread_id) DO UPDATE SET
    scope_key = COALESCE(thread_memory_scope.scope_key, excluded.scope_key),
    may_write_global = min(thread_memory_scope.may_write_global, excluded.may_write_global)
            "#,
        )
        .bind(thread_id.to_string())
        .bind(scope_key)
        .bind(i64::from(may_write_global))
        .execute(self.pool.as_ref())
        .await?;
        Ok(())
    }

    /// Returns the registered memory scope of a thread, if any.
    pub async fn thread_memory_scope(
        &self,
        thread_id: ThreadId,
    ) -> anyhow::Result<Option<ThreadMemoryScope>> {
        let row = sqlx::query(
            "SELECT scope_key, may_write_global FROM thread_memory_scope WHERE thread_id = ?",
        )
        .bind(thread_id.to_string())
        .fetch_optional(self.pool.as_ref())
        .await?;
        row.map(|row| -> anyhow::Result<ThreadMemoryScope> {
            let may_write_global: i64 = row.try_get("may_write_global")?;
            Ok(ThreadMemoryScope {
                scope_key: row.try_get("scope_key")?,
                may_write_global: may_write_global != 0,
            })
        })
        .transpose()
    }

    /// Whether any thread registered a private memory scope.
    pub async fn has_memory_scopes(&self) -> anyhow::Result<bool> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM thread_memory_scope WHERE scope_key IS NOT NULL",
        )
        .fetch_one(self.pool.as_ref())
        .await?;
        Ok(count > 0)
    }

    /// Same as [`Self::mark_stage1_job_succeeded`], but stores the output in
    /// `partition` and enqueues that partition's Phase 2 job.
    #[allow(clippy::too_many_arguments)]
    pub async fn mark_stage1_job_succeeded_in_partition(
        &self,
        thread_id: ThreadId,
        ownership_token: &str,
        source_updated_at: i64,
        raw_memory: &str,
        rollout_summary: &str,
        rollout_slug: Option<&str>,
        partition: &str,
    ) -> anyhow::Result<bool> {
        let now = Utc::now().timestamp();
        let thread_id = thread_id.to_string();

        let mut tx = self.pool.begin().await?;
        let rows_affected = sqlx::query(
            r#"
UPDATE jobs
SET
    status = 'done',
    finished_at = ?,
    lease_until = NULL,
    last_error = NULL,
    last_success_watermark = input_watermark
WHERE kind = ? AND job_key = ?
  AND status = 'running' AND ownership_token = ?
            "#,
        )
        .bind(now)
        .bind(JOB_KIND_MEMORY_STAGE1)
        .bind(thread_id.as_str())
        .bind(ownership_token)
        .execute(&mut *tx)
        .await?
        .rows_affected();

        if rows_affected == 0 {
            tx.commit().await?;
            return Ok(false);
        }

        // A snapshot that moves between partitions must leave the previous
        // partition's consolidation baseline.
        let previous_partition: Option<String> =
            sqlx::query_scalar("SELECT memory_partition FROM stage1_outputs WHERE thread_id = ?")
                .bind(thread_id.as_str())
                .fetch_optional(&mut *tx)
                .await?;

        sqlx::query(
            r#"
INSERT INTO stage1_outputs (
    thread_id,
    source_updated_at,
    raw_memory,
    rollout_summary,
    rollout_slug,
    generated_at,
    memory_partition
) VALUES (?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(thread_id) DO UPDATE SET
    source_updated_at = excluded.source_updated_at,
    raw_memory = excluded.raw_memory,
    rollout_summary = excluded.rollout_summary,
    rollout_slug = excluded.rollout_slug,
    generated_at = excluded.generated_at,
    memory_partition = excluded.memory_partition
WHERE excluded.source_updated_at >= stage1_outputs.source_updated_at
            "#,
        )
        .bind(thread_id.as_str())
        .bind(source_updated_at)
        .bind(raw_memory)
        .bind(rollout_summary)
        .bind(rollout_slug)
        .bind(now)
        .bind(partition)
        .execute(&mut *tx)
        .await?;

        enqueue_consolidation_for_partition_with_executor(&mut *tx, source_updated_at, partition)
            .await?;
        if let Some(previous_partition) = previous_partition
            && previous_partition != partition
        {
            enqueue_consolidation_for_partition_with_executor(
                &mut *tx,
                source_updated_at,
                previous_partition.as_str(),
            )
            .await?;
        }

        tx.commit().await?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::super::StateRuntime;
    use super::super::test_support::test_thread_metadata;
    use super::super::test_support::unique_temp_dir;
    use super::*;
    use crate::model::Stage1JobClaimOutcome;
    use codex_utils_absolute_path::test_support::PathExt;
    use pretty_assertions::assert_eq;

    async fn runtime_with_thread() -> (Arc<StateRuntime>, ThreadId, std::path::PathBuf) {
        let codex_home = unique_temp_dir();
        let runtime = StateRuntime::init(
            crate::SqliteConfig::new_for_testing(codex_home.as_path().abs()),
            "test-provider".to_string(),
        )
        .await
        .expect("initialize runtime");
        let thread_id = ThreadId::from_string(&Uuid::new_v4().to_string()).expect("thread id");
        let metadata = test_thread_metadata(&codex_home, thread_id, codex_home.join("a"));
        runtime
            .upsert_thread(&metadata)
            .await
            .expect("upsert thread");
        (runtime, thread_id, codex_home)
    }

    async fn succeed_in_partition(
        store: &MemoryStore,
        thread_id: ThreadId,
        source_updated_at: i64,
        partition: &str,
    ) {
        let owner = ThreadId::from_string(&Uuid::new_v4().to_string()).expect("owner id");
        let claim = store
            .try_claim_stage1_job(thread_id, owner, source_updated_at, 3600, 64)
            .await
            .expect("claim stage1 job");
        let Stage1JobClaimOutcome::Claimed { ownership_token } = claim else {
            panic!("unexpected claim outcome: {claim:?}");
        };
        assert!(
            store
                .mark_stage1_job_succeeded_in_partition(
                    thread_id,
                    &ownership_token,
                    source_updated_at,
                    "raw",
                    "summary",
                    None,
                    partition,
                )
                .await
                .expect("mark succeeded")
        );
    }

    #[tokio::test]
    async fn thread_scope_upsert_accumulates_may_write_global() {
        let (runtime, thread_id, codex_home) = runtime_with_thread().await;
        let store = runtime.memories();
        assert_eq!(store.thread_memory_scope(thread_id).await.unwrap(), None);
        assert!(!store.has_memory_scopes().await.unwrap());

        store
            .upsert_thread_memory_scope(thread_id, Some("chat"), false)
            .await
            .unwrap();
        store
            .upsert_thread_memory_scope(thread_id, None, true)
            .await
            .unwrap();
        store
            .upsert_thread_memory_scope(thread_id, Some("other-chat"), true)
            .await
            .unwrap();
        assert_eq!(
            store.thread_memory_scope(thread_id).await.unwrap(),
            Some(ThreadMemoryScope {
                scope_key: Some("chat".to_string()),
                may_write_global: false,
            })
        );
        assert!(store.has_memory_scopes().await.unwrap());
        let _ = tokio::fs::remove_dir_all(codex_home).await;
    }

    #[tokio::test]
    async fn phase2_selection_and_baseline_are_partitioned() {
        let (runtime, thread_id, codex_home) = runtime_with_thread().await;
        let store = runtime.memories();
        let now = Utc::now().timestamp();
        succeed_in_partition(store, thread_id, now, "scope:chat").await;

        assert!(
            store
                .get_phase2_input_selection(10, 30)
                .await
                .unwrap()
                .is_empty(),
            "scoped outputs must never reach the global selection"
        );
        assert!(
            store
                .list_stage1_outputs_for_global(10)
                .await
                .unwrap()
                .is_empty()
        );
        let scoped = store
            .get_phase2_input_selection_for_partition(10, 30, "scope:chat")
            .await
            .unwrap();
        assert_eq!(scoped.len(), 1);

        // Claim and finish the scope job; the global job is independent.
        let worker = ThreadId::from_string(&Uuid::new_v4().to_string()).expect("worker id");
        let Phase2JobClaimOutcome::Claimed {
            ownership_token, ..
        } = store
            .try_claim_phase2_job_for_partition(worker, 3600, "scope:chat", false)
            .await
            .unwrap()
        else {
            panic!("scope job should be claimable");
        };
        assert!(matches!(
            store
                .try_claim_global_phase2_job(worker, 3600)
                .await
                .unwrap(),
            Phase2JobClaimOutcome::Claimed { .. }
        ));
        assert!(
            store
                .mark_phase2_job_succeeded_for_partition(
                    &ownership_token,
                    now,
                    &scoped,
                    "scope:chat"
                )
                .await
                .unwrap()
        );
        assert_eq!(
            store
                .try_claim_phase2_job_for_partition(worker, 3600, "scope:chat", false)
                .await
                .unwrap(),
            Phase2JobClaimOutcome::SkippedCooldown
        );
        assert!(matches!(
            store
                .try_claim_phase2_job_for_partition(worker, 3600, "scope:chat", true)
                .await
                .unwrap(),
            Phase2JobClaimOutcome::Claimed { .. }
        ));
        let selected: i64 = sqlx::query_scalar(
            "SELECT selected_for_phase2 FROM stage1_outputs WHERE thread_id = ?",
        )
        .bind(thread_id.to_string())
        .fetch_one(store.pool.as_ref())
        .await
        .unwrap();
        assert_eq!(selected, 1);
        let _ = tokio::fs::remove_dir_all(codex_home).await;
    }
}
