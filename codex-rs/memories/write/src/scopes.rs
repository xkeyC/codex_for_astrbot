//! Fork addition: per-chat memory scopes and manual consolidation.
//!
//! Everything here is inert unless a thread sets `memories.scope_key`,
//! `memories.may_write_global = false`, or `memories.auto_consolidate = false`.
//!
//! - Threads with a scope register a `thread_memory_scope` row.
//! - Phase 1 asks the extraction model to classify each scoped thread's
//!   memory as `global` or `private`; only `global` output of threads that may
//!   write globally reaches the global partition, everything else lands in the
//!   thread's `scope:<key>` partition.
//! - Phase 2 consolidates each partition into its own root
//!   (`<codex_home>/<memories dir>_scopes/<scope dir>/` for scopes).

use crate::ensure_layout;
use crate::extensions::seed_extension_instructions;
use crate::guard;
use crate::metrics::MEMORY_STARTUP;
use crate::phase1;
use crate::phase2;
use crate::runtime::MemoryStartupContext;
use codex_config::memory_scopes::GLOBAL_MEMORY_PARTITION;
use codex_config::memory_scopes::memory_scope_partition;
use codex_config::memory_scopes::memory_scope_root;
use codex_config::types::MemoriesConfig;
use codex_core::CodexThread;
use codex_core::ThreadManager;
use codex_core::config::Config;
use codex_features::Feature;
use codex_login::AuthManager;
use codex_protocol::MemoryVersion;
use codex_protocol::ThreadId;
use codex_protocol::models::PermissionProfile;
use codex_protocol::protocol::SessionSource;
use codex_protocol::user_input::UserInput;
use codex_state::ThreadMemoryScope;
use codex_utils_absolute_path::AbsolutePathBuf;
use serde_json::Value;
use std::sync::Arc;
use tracing::warn;

const VISIBILITY_FIELD: &str = "memory_visibility";

const CLASSIFICATION_INSTRUCTIONS: &str = r#"

## Memory visibility (required field `memory_visibility`)

This conversation belongs to one chat of a multi-chat assistant. Memories are
stored either in a GLOBAL store shared with every other chat and person, or in
a PRIVATE store visible only inside this chat.

Set `memory_visibility` to "global" ONLY if every statement in your output is
general, impersonal knowledge that is safe for anyone in any other chat to see
(for example tool usage, workflows, public facts, general conventions).

Set it to "private" if the output contains ANY information about a person or
this chat, including: names, nicknames, user or group identifiers, contact
information, relationships, health, finances, locations, schedules, personal
preferences or opinions of specific users, and anything internal to this group
or conversation. When in doubt, choose "private".
"#;

const GLOBAL_CONSOLIDATION_ADDENDUM: &str = r#"

## Shared memory store

This memory folder is shared across all chats and all people. Keep only general,
impersonal knowledge. Remove or never write anything that identifies or
describes a specific person, user, group, or chat (identifiers, contact info,
relationships, health, finances, locations, per-user preferences, group-internal
matters), even if it appears in the inputs.
"#;

const SCOPE_CONSOLIDATION_ADDENDUM: &str = r#"

## Private chat memory store

This memory folder is private to a single chat. It may contain information about
the people in that chat. Only consolidate the inputs provided here; do not
reference or copy content from other memory folders.
"#;

/// How Phase 1 routes the output of one claimed thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ThreadRouting {
    /// No scope registered: upstream behavior (global partition).
    Upstream,
    /// Private-only thread without a scope: nothing may be stored.
    Drop,
    /// Scoped thread; `classify` asks the model for a visibility decision.
    Scoped { scope_key: String, classify: bool },
}

impl ThreadRouting {
    pub(crate) fn from_scope(scope: Option<ThreadMemoryScope>) -> Self {
        match scope {
            None => Self::Upstream,
            Some(ThreadMemoryScope {
                scope_key: Some(scope_key),
                may_write_global,
            }) => Self::Scoped {
                scope_key,
                classify: may_write_global,
            },
            Some(ThreadMemoryScope {
                scope_key: None,
                may_write_global: false,
            }) => Self::Drop,
            Some(ThreadMemoryScope {
                scope_key: None,
                may_write_global: true,
            }) => Self::Upstream,
        }
    }

    pub(crate) fn classify(&self) -> bool {
        matches!(self, Self::Scoped { classify: true, .. })
    }

    /// Partition for the output, or `None` for the upstream global path.
    /// Only an explicit `global` verdict of a thread that may write globally
    /// reaches the global partition.
    pub(crate) fn partition(&self, visibility: Option<Visibility>) -> Option<String> {
        match self {
            Self::Upstream | Self::Drop => None,
            Self::Scoped {
                scope_key,
                classify,
            } => Some(if *classify && visibility == Some(Visibility::Global) {
                GLOBAL_MEMORY_PARTITION.to_string()
            } else {
                memory_scope_partition(scope_key)
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Visibility {
    Global,
    Private,
}

pub(crate) async fn thread_routing(
    context: &MemoryStartupContext,
    thread_id: ThreadId,
) -> ThreadRouting {
    let Some(db) = context.memory_store().await else {
        return ThreadRouting::Upstream;
    };
    match db.thread_memory_scope(thread_id).await {
        Ok(scope) => ThreadRouting::from_scope(scope),
        Err(err) => {
            // Fail closed: without knowing the scope, store nothing.
            warn!("failed reading memory scope for thread {thread_id}: {err}");
            ThreadRouting::Drop
        }
    }
}

/// Adds the visibility classification to a Phase 1 prompt.
pub(crate) fn add_classification(instructions: &mut String, schema: &mut Value) {
    instructions.push_str(CLASSIFICATION_INSTRUCTIONS);
    if let Some(properties) = schema.get_mut("properties").and_then(Value::as_object_mut) {
        properties.insert(
            VISIBILITY_FIELD.to_string(),
            serde_json::json!({ "type": "string", "enum": ["global", "private"] }),
        );
    }
    if let Some(required) = schema.get_mut("required").and_then(Value::as_array_mut) {
        required.push(Value::String(VISIBILITY_FIELD.to_string()));
    }
}

/// Removes the visibility field from a Phase 1 response so the upstream
/// parser accepts it. Missing or unknown values mean private.
pub(crate) fn split_visibility(source: &str) -> (String, Visibility) {
    let Ok(Value::Object(mut object)) = serde_json::from_str::<Value>(source) else {
        return (source.to_string(), Visibility::Private);
    };
    let visibility = match object.remove(VISIBILITY_FIELD) {
        Some(Value::String(value)) if value == "global" => Visibility::Global,
        _ => Visibility::Private,
    };
    (Value::Object(object).to_string(), visibility)
}

pub(crate) async fn mark_success_in_partition(
    context: &MemoryStartupContext,
    claim: (ThreadId, &str, i64),
    output: (&str, &str, Option<&str>),
    partition: &str,
) -> bool {
    let (thread_id, ownership_token, source_updated_at) = claim;
    let (raw_memory, rollout_summary, rollout_slug) = output;
    let Some(db) = context.memory_store().await else {
        return false;
    };
    db.mark_stage1_job_succeeded_in_partition(
        thread_id,
        ownership_token,
        source_updated_at,
        raw_memory,
        rollout_summary,
        rollout_slug,
        partition,
    )
    .await
    .unwrap_or(false)
}

/// Phase 2 consolidation target: one partition and its filesystem root.
#[derive(Debug, Clone)]
pub(crate) struct Phase2Target {
    pub(crate) partition: String,
    pub(crate) root: AbsolutePathBuf,
    pub(crate) scoped: bool,
    pub(crate) skip_cooldown: bool,
}

impl Phase2Target {
    pub(crate) fn global(config: &Config, skip_cooldown: bool) -> Self {
        Self {
            partition: GLOBAL_MEMORY_PARTITION.to_string(),
            root: config
                .codex_home
                .join(config.memories.version.directory_name()),
            scoped: false,
            skip_cooldown,
        }
    }

    pub(crate) fn scope(config: &Config, scope_key: &str, skip_cooldown: bool) -> Self {
        Self {
            partition: memory_scope_partition(scope_key),
            root: scope_root(config, scope_key),
            scoped: true,
            skip_cooldown,
        }
    }
}

pub(crate) fn scope_root(config: &Config, scope_key: &str) -> AbsolutePathBuf {
    memory_scope_root(
        &config.codex_home,
        config.memories.version.directory_name(),
        scope_key,
    )
}

/// Whether any fork-only memory option is set on this config.
pub(crate) fn fork_mode_active(memories: &MemoriesConfig) -> bool {
    memories.scope_key.is_some()
        || !memories.may_write_global
        || !memories.auto_consolidate
        || !memories.extra_session_sources.is_empty()
}

/// Consolidation agents must not inherit a host persona from the triggering
/// thread (e.g. a chatbot's base instructions), and they need a plain
/// file-editing tool surface even when the host denies its chat threads one.
pub(crate) fn adjust_agent_config(agent_config: &mut Config, parent: &Config) {
    if !fork_mode_active(&parent.memories) {
        return;
    }
    agent_config.base_instructions = None;
    agent_config.developer_instructions = None;
    restore_file_editing_surface(agent_config);
}

/// Gives the consolidation agent back the tools its prompt assumes.
///
/// A host like AstrBot runs its chat threads with no shell (`features
/// .shell_tool = false`) and in code mode, because Codex is an orchestrator
/// there and must stay off the machine's filesystem. Consolidation is the one
/// session where that is backwards: its entire job is to read the workspace
/// diff and rewrite the files under the memory root, and the sandbox already
/// confines it to exactly that directory (see `phase2::agent::
/// get_config_for_root`, which narrows the policy to `writable_roots =
/// [memory root]` with no network). Without this the agent is handed a
/// consolidation prompt and no way to carry it out, so memory files silently
/// stop being maintained.
fn restore_file_editing_surface(agent_config: &mut Config) {
    // Code mode defers every tool behind a JS host process; the consolidation
    // prompt expects to call the tools directly.
    agent_config.model_tool_mode = None;
    if let Err(err) = agent_config.features.enable(Feature::ShellTool) {
        // A managed policy can pin the feature off. Say so instead of leaving
        // an agent that cannot touch the files it was asked to rewrite.
        warn!("memory consolidation could not enable the shell tool: {err}");
    }
    // With a shell back, the agent needs to know which platform it is on to
    // pick working commands.
    agent_config.include_environment_context = true;
}

/// Appends scope rules to the consolidation prompt.
pub(crate) async fn extend_consolidation_prompt(
    context: &MemoryStartupContext,
    target: &Phase2Target,
    prompt: &mut [UserInput],
) {
    let addendum = if target.scoped {
        SCOPE_CONSOLIDATION_ADDENDUM
    } else {
        let has_scopes = match context.memory_store().await {
            Some(db) => db.has_memory_scopes().await.unwrap_or(true),
            None => true,
        };
        if !has_scopes {
            return;
        }
        GLOBAL_CONSOLIDATION_ADDENDUM
    };
    if let Some(UserInput::Text { text, .. }) = prompt.first_mut() {
        text.push_str(addendum);
    }
}

/// Registers the thread scope and prepares its root. Returns `false` when
/// automatic Phase 1 / Phase 2 runs are disabled for this thread.
pub(crate) async fn prepare_startup(context: &MemoryStartupContext, config: &Config) -> bool {
    prepare_scope(context, config).await;
    config.memories.auto_consolidate
}

async fn prepare_scope(context: &MemoryStartupContext, config: &Config) {
    let memories = &config.memories;
    if memories.scope_key.is_none() && memories.may_write_global {
        return;
    }
    if let Some(db) = context.memory_store().await
        && let Err(err) = db
            .upsert_thread_memory_scope(
                context.thread_id(),
                memories.scope_key.as_deref(),
                memories.may_write_global,
            )
            .await
    {
        warn!("failed registering memory scope: {err}");
    }
    if let Some(scope_key) = memories.scope_key.as_deref() {
        let root = scope_root(config, scope_key);
        if let Err(err) = ensure_layout(&root).await {
            warn!("failed preparing memory scope root: {err}");
            return;
        }
        if let Err(err) = seed_extension_instructions(&root).await {
            warn!("failed seeding memory scope extension instructions: {err}");
        }
    }
}

/// Runs Phase 2 for the scope of the triggering thread, if it has one.
pub(crate) async fn run_scope_phase2(
    context: Arc<MemoryStartupContext>,
    config: Arc<Config>,
    parent_permission_profile: PermissionProfile,
    skip_cooldown: bool,
) {
    let Some(scope_key) = config.memories.scope_key.clone() else {
        return;
    };
    let target = Phase2Target::scope(&config, &scope_key, skip_cooldown);
    phase2::run_for_target(context, config, parent_permission_profile, target).await;
}

/// Runs memory extraction (Phase 1) and consolidation (Phase 2) right now,
/// regardless of `memories.auto_consolidate`.
///
/// `force` skips the Phase 2 success cooldown and the Phase 1 rollout idle
/// requirement. Consolidation of the global store and of the thread's scope
/// (when `memories.scope_key` is set) is dispatched; the consolidation agents
/// keep running in the background after this returns.
#[allow(clippy::too_many_arguments)]
pub async fn run_memories_consolidation_now(
    thread_manager: Arc<ThreadManager>,
    auth_manager: Arc<AuthManager>,
    thread_id: ThreadId,
    thread: Arc<CodexThread>,
    config: Arc<Config>,
    parent_permission_profile: PermissionProfile,
    source: &SessionSource,
    force: bool,
) -> anyhow::Result<()> {
    if config.ephemeral || !config.features.enabled(Feature::MemoryTool) {
        anyhow::bail!("memories are disabled for this thread");
    }
    let versions = if config.memories.dual_write {
        vec![MemoryVersion::V1, MemoryVersion::V2]
    } else {
        vec![config.memories.version]
    };
    for version in versions {
        let mut pipeline_config = config.as_ref().clone();
        pipeline_config.memories.version = version;
        if force {
            pipeline_config.memories.min_rollout_idle_hours = 0;
        }
        let config = Arc::new(pipeline_config);
        let context = Arc::new(MemoryStartupContext::new(
            Arc::clone(&thread_manager),
            Arc::clone(&auth_manager),
            thread_id,
            Arc::clone(&thread),
            config.as_ref(),
            source.clone(),
        ));
        if context.memory_store().await.is_none() {
            anyhow::bail!("state db unavailable for memories");
        }
        let root = config
            .codex_home
            .join(config.memories.version.directory_name());
        ensure_layout(&root).await?;
        if let Err(err) = seed_extension_instructions(&root).await {
            warn!("failed seeding memory extension instructions: {err}");
        }
        prepare_scope(context.as_ref(), &config).await;
        phase1::prune(context.as_ref(), &config).await;
        if !guard::rate_limits_ok(&auth_manager, &config).await {
            context.counter(
                MEMORY_STARTUP,
                /*inc*/ 1,
                &[("status", "skipped_rate_limit")],
            );
            anyhow::bail!("memory consolidation skipped: rate limits too low");
        }
        phase1::run(Arc::clone(&context), Arc::clone(&config)).await;
        phase2::run_for_target(
            Arc::clone(&context),
            Arc::clone(&config),
            parent_permission_profile.clone(),
            Phase2Target::global(&config, force),
        )
        .await;
        run_scope_phase2(context, config, parent_permission_profile.clone(), force).await;
    }
    Ok(())
}

#[cfg(test)]
#[path = "scopes_tests.rs"]
mod tests;
