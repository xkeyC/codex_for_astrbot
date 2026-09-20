mod backend;
mod extension;
mod local;
// Fork addition: memory-file maintenance for a consolidation agent.
mod maintenance;
mod metrics;
mod prompts;
mod schema;
// Fork addition: per-chat memory scopes.
mod scoped;
mod tools;

pub use extension::install;

pub(crate) const DEFAULT_LIST_MAX_RESULTS: usize = 2_000;
pub(crate) const MAX_LIST_RESULTS: usize = 2_000;
pub(crate) const DEFAULT_SEARCH_MAX_RESULTS: usize = 200;
pub(crate) const MAX_SEARCH_RESULTS: usize = 200;
pub(crate) const DEFAULT_READ_MAX_TOKENS: usize = 20_000;
pub(crate) const MEMORY_TOOL_DEVELOPER_INSTRUCTIONS_SUMMARY_TOKEN_LIMIT: usize = 2_500;

pub(crate) const MEMORY_TOOLS_NAMESPACE: &str = "memories";
pub(crate) const ADD_AD_HOC_NOTE_TOOL_NAME: &str = "add_ad_hoc_note";
pub(crate) const LIST_TOOL_NAME: &str = "list";
pub(crate) const READ_TOOL_NAME: &str = "read";
pub(crate) const SEARCH_TOOL_NAME: &str = "search";
// Fork addition: entry-level deletion tool, gated by `memories.may_delete`.
pub(crate) const DELETE_TOOL_NAME: &str = "delete_memory";
// Fork addition: file write, gated by `memories.maintenance_tools`.
pub(crate) const WRITE_TOOL_NAME: &str = "write";

#[cfg(test)]
mod tests;

// Fork addition: tests for entry-level memory deletion.
#[cfg(test)]
#[path = "delete_tests.rs"]
mod delete_tests;

// Fork addition: tests for the consolidation agent's memory-file tools.
#[cfg(test)]
#[path = "maintenance_tests.rs"]
mod maintenance_tests;
