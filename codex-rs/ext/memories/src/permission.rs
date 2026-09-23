//! Fork addition: memory permissions of the current turn's sender.
//!
//! A chat shared by many people (a group) runs one thread, so whether shared
//! memories may be written, or any memory deleted, belongs to whoever sent the
//! current turn, not to the thread. A thread with `memories.turn_scopes`
//! registers the curating tools for everyone (its tool set and prompt stay the
//! same whoever speaks, keeping the prompt cache) and checks, when a tool
//! runs, the permission scopes the host sent with that turn.

use codex_extension_api::ToolCall;

/// Scope allowing notes in, and deletions from, the shared store.
pub(crate) const WRITE_GLOBAL_SCOPE: &str = "memory.write_global";
/// Scope allowing memories to be deleted.
pub(crate) const DELETE_SCOPE: &str = "memory.delete";

/// Whether a curating action is allowed: fixed by the thread's config, or up
/// to a scope of the current turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Permission {
    Fixed(bool),
    Scope(&'static str),
}

impl Permission {
    pub(crate) fn allowed(&self, call: &ToolCall<'_>) -> bool {
        match self {
            Self::Fixed(allowed) => *allowed,
            Self::Scope(scope) => call.scopes.iter().any(|granted| granted == scope),
        }
    }
}
