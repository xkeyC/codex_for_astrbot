//! Fork addition: read path for per-chat memory scopes.
//!
//! A thread with `memories.scope_key` sees the global memory store plus its
//! own scope (`<codex_home>/<memories dir>_scopes/<scope dir>/`), never other
//! scopes. The memory tools address the two roots through `global/...` and
//! `local/...` paths. Threads without a scope key are unaffected.

use std::sync::Arc;

use codex_config::memory_scopes::memory_scope_root;
use codex_core::config::Config;
use codex_extension_api::ToolCall;
use codex_extension_api::ToolExecutor;
use codex_otel::MetricsClient;
use codex_protocol::MemoryVersion;
use codex_utils_absolute_path::AbsolutePathBuf;

use crate::ADD_AD_HOC_NOTE_TOOL_NAME;
use crate::DELETE_TOOL_NAME;
use crate::MEMORY_TOOLS_NAMESPACE;
use crate::backend::AddAdHocMemoryNoteRequest;
use crate::backend::AddAdHocMemoryNoteResponse;
use crate::backend::DeleteMemoryRequest;
use crate::backend::DeleteMemoryResponse;
use crate::backend::ListMemoriesRequest;
use crate::backend::ListMemoriesResponse;
use crate::backend::MemoriesBackend;
use crate::backend::MemoriesBackendError;
use crate::backend::MemoryEntry;
use crate::backend::MemoryEntryType;
use crate::backend::ReadMemoryRequest;
use crate::backend::ReadMemoryResponse;
use crate::backend::SearchMemoriesRequest;
use crate::backend::SearchMemoriesResponse;
use crate::local::LocalMemoriesBackend;
use crate::prompts::build_memory_instructions_for_root;
use crate::prompts::build_memory_tool_developer_instructions;
use crate::prompts::read_memory_summary;
use crate::tools;

type MemoryTool = Arc<dyn for<'call> ToolExecutor<ToolCall<'call>>>;

const GLOBAL_PREFIX: &str = "global";
const LOCAL_PREFIX: &str = "local";

/// Scope settings of a thread, stored next to `MemoriesExtensionConfig`.
#[derive(Clone, Debug, Default)]
pub(crate) struct ScopedMemoriesConfig {
    pub(crate) scope_key: Option<String>,
    pub(crate) may_write_global: bool,
    pub(crate) may_delete: bool,
}

impl ScopedMemoriesConfig {
    pub(crate) fn from_config(config: &Config) -> Self {
        Self {
            scope_key: config.memories.scope_key.clone(),
            may_write_global: config.memories.may_write_global,
            may_delete: config.memories.may_delete,
        }
    }
}

pub(crate) fn scope_root(
    codex_home: &AbsolutePathBuf,
    version: MemoryVersion,
    scope_key: &str,
) -> AbsolutePathBuf {
    memory_scope_root(codex_home, version.directory_name(), scope_key)
}

const NO_GLOBAL_WRITE_INSTRUCTIONS: &str = "\n\n## Shared memory is read-only in this chat\n\n\
This memory folder is SHARED with every other chat. This chat may not write to \
it: do not create ad-hoc update notes or any other file under it, even when \
asked to remember something. Tell the user that memories cannot be saved here.\n";

/// Fork addition: appended when `delete_memory` is registered on a thread
/// without a memory scope.
const DELETE_TOOL_INSTRUCTIONS: &str = "\n\n## Deleting memories\n\n\
The `memories` tools include `delete_memory`, which permanently deletes one \
memory file by relative path. Use it only when the user explicitly asks to \
delete or forget a stored memory, and delete nothing else.\n";

/// Read-path developer instructions for a thread.
///
/// Threads without fork scope settings get the upstream instructions unchanged.
pub(crate) async fn developer_instructions(
    codex_home: &AbsolutePathBuf,
    version: MemoryVersion,
    scope: Option<&ScopedMemoriesConfig>,
    dedicated_tools: bool,
) -> Option<String> {
    match scope {
        Some(scope) if scope.scope_key.is_some() => {
            build_scoped_developer_instructions(codex_home, version, scope, dedicated_tools).await
        }
        Some(scope) if !scope.may_write_global => {
            let mut text = build_memory_tool_developer_instructions(codex_home, version).await?;
            text.push_str(NO_GLOBAL_WRITE_INSTRUCTIONS);
            Some(text)
        }
        // Fork addition: mention `delete_memory` only where it is registered.
        Some(scope) if dedicated_tools && scope.may_delete => {
            let mut text = build_memory_tool_developer_instructions(codex_home, version).await?;
            text.push_str(DELETE_TOOL_INSTRUCTIONS);
            Some(text)
        }
        _ => build_memory_tool_developer_instructions(codex_home, version).await,
    }
}

/// Developer instructions covering the global store and the thread's scope.
pub(crate) async fn build_scoped_developer_instructions(
    codex_home: &AbsolutePathBuf,
    version: MemoryVersion,
    scope: &ScopedMemoriesConfig,
    dedicated_tools: bool,
) -> Option<String> {
    let scope_key = scope.scope_key.as_deref()?;
    let local_root = scope_root(codex_home, version, scope_key);
    let global = build_memory_tool_developer_instructions(codex_home, version).await;
    let local_summary = read_memory_summary(&local_root).await;

    let (mut text, global_described) = match (global, local_summary.as_ref()) {
        (None, None) => return None,
        (Some(global), _) => (global, true),
        (None, Some(_)) => (
            build_memory_instructions_for_root(&local_root, version).await?,
            false,
        ),
    };
    let intro = if global_described {
        format!(
            "This chat also has a PRIVATE memory folder at {}. The memory folder \
described above is SHARED with every other chat; the private folder is visible \
only in this chat and may contain information about the people here.",
            local_root.display()
        )
    } else {
        format!(
            "The memory folder described above is PRIVATE to this chat and may \
contain information about the people here. The SHARED memory folder at {} is \
visible to every other chat.",
            codex_home.join(version.directory_name()).display()
        )
    };
    text.push_str(&format!(
        "\n\n## Chat-private memories\n\n{intro}\n\n\
- Never reveal private memories outside this chat and never copy personal \
information (identifiers, contact info, relationships, health, finances, \
locations, per-user preferences, group-internal matters) into shared memory.\n\
- Memory update notes requested by the user go to \
{local_notes}/, never to the shared folder's notes directory.\n",
        local_notes = local_root
            .join("extensions")
            .join("ad_hoc")
            .join("notes")
            .display(),
    ));
    if !scope.may_write_global {
        text.push_str("- This chat may not write to the shared folder at all.\n");
    }
    if dedicated_tools {
        text.push_str(&format!(
            "- With the `{MEMORY_TOOLS_NAMESPACE}` tools, paths starting with `{GLOBAL_PREFIX}/` \
address the shared folder and paths starting with `{LOCAL_PREFIX}/` address the private folder.\n"
        ));
        if scope.may_write_global {
            text.push_str(&format!(
                "- `{ADD_AD_HOC_NOTE_TOOL_NAME}` writes to the private folder by default; pass \
`scope: \"global\"` only for impersonal knowledge useful in every chat.\n"
            ));
        } else {
            text.push_str(&format!(
                "- `{ADD_AD_HOC_NOTE_TOOL_NAME}` writes to the private folder; this chat cannot \
write shared memories.\n"
            ));
        }
        if scope.may_delete {
            if scope.may_write_global {
                text.push_str(&format!(
                    "- `{DELETE_TOOL_NAME}` permanently deletes one memory file at the given \
`{GLOBAL_PREFIX}/` or `{LOCAL_PREFIX}/` path; use it only when the user asks to delete or forget \
a stored memory.\n"
                ));
            } else {
                text.push_str(&format!(
                    "- `{DELETE_TOOL_NAME}` permanently deletes one `{LOCAL_PREFIX}/` memory file \
when the user asks to delete or forget it; shared memories cannot be deleted here.\n"
                ));
            }
        }
    }
    if let Some(local_summary) = local_summary {
        text.push_str("\n========= PRIVATE MEMORY_SUMMARY BEGINS =========\n");
        text.push_str(&local_summary);
        text.push_str("\n========= PRIVATE MEMORY_SUMMARY ENDS =========\n");
    }
    Some(text)
}

/// Memory tools for a thread with fork scope settings, or `None` for the
/// upstream tools.
pub(crate) fn scoped_memory_tools(
    codex_home: &AbsolutePathBuf,
    version: MemoryVersion,
    scope: &ScopedMemoriesConfig,
    metrics_client: Option<MetricsClient>,
) -> Option<Vec<MemoryTool>> {
    let ad_hoc_name = codex_extension_api::ToolName::namespaced(
        MEMORY_TOOLS_NAMESPACE,
        ADD_AD_HOC_NOTE_TOOL_NAME,
    );
    let global = LocalMemoriesBackend::from_memory_root(
        codex_home.join(version.directory_name()).to_path_buf(),
    );
    let Some(scope_key) = scope.scope_key.as_deref() else {
        if scope.may_write_global {
            if !scope.may_delete {
                return None;
            }
            // No private store, but this thread may curate the single store.
            let mut memory_tools = tools::memory_tools(global.clone(), metrics_client.clone());
            memory_tools.push(Arc::new(tools::DeleteMemoryTool {
                backend: global,
                may_delete_global: true,
                metrics_client,
            }));
            return Some(memory_tools);
        }
        // No private store and no global write permission: read-only tools.
        let mut memory_tools = tools::memory_tools(global, metrics_client);
        memory_tools.retain(|tool| tool.tool_name() != ad_hoc_name);
        return Some(memory_tools);
    };
    let local = LocalMemoriesBackend::from_memory_root(
        scope_root(codex_home, version, scope_key).to_path_buf(),
    );
    let backend = ScopedMemoriesBackend {
        global: global.clone(),
        local: local.clone(),
    };
    let mut memory_tools = tools::memory_tools(backend.clone(), metrics_client.clone());
    if scope.may_write_global {
        memory_tools.retain(|tool| tool.tool_name() != ad_hoc_name);
        memory_tools.insert(
            0,
            Arc::new(tools::ScopedAddAdHocNoteTool {
                local,
                global,
                metrics_client: metrics_client.clone(),
            }),
        );
    }
    if scope.may_delete {
        memory_tools.push(Arc::new(tools::DeleteMemoryTool {
            backend,
            may_delete_global: scope.may_write_global,
            metrics_client,
        }));
    }
    Some(memory_tools)
}

/// Routes `global/...` and `local/...` paths to the two memory roots.
#[derive(Debug, Clone)]
pub(crate) struct ScopedMemoriesBackend {
    pub(crate) global: LocalMemoriesBackend,
    pub(crate) local: LocalMemoriesBackend,
}

#[derive(Debug)]
enum Route<'a> {
    Root,
    Global(Option<&'a str>),
    Local(Option<&'a str>),
}

fn route(path: Option<&str>) -> Result<Route<'_>, MemoriesBackendError> {
    let Some(path) = path else {
        return Ok(Route::Root);
    };
    let trimmed = path.trim_start_matches("./");
    if trimmed.is_empty() || trimmed == "." {
        return Ok(Route::Root);
    }
    let (head, rest) = match trimmed.find(['/', '\\']) {
        Some(index) => (&trimmed[..index], Some(&trimmed[index + 1..])),
        None => (trimmed, None),
    };
    let rest = rest.filter(|rest| !rest.is_empty());
    match head {
        GLOBAL_PREFIX => Ok(Route::Global(rest)),
        LOCAL_PREFIX => Ok(Route::Local(rest)),
        _ => Err(MemoriesBackendError::invalid_path(
            path,
            "must start with `global/` (shared memories) or `local/` (this chat's memories)",
        )),
    }
}

/// Whether a tool path addresses the shared (global) memory store.
pub(crate) fn addresses_global_store(path: &str) -> bool {
    matches!(route(Some(path)), Ok(Route::Global(_)))
}

fn prefixed(prefix: &str, path: &str) -> String {
    if path.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix}/{}", path.replace('\\', "/"))
    }
}

fn prefix_error(prefix: &str, err: MemoriesBackendError) -> MemoriesBackendError {
    match err {
        MemoriesBackendError::NotFound { path } => MemoriesBackendError::NotFound {
            path: prefixed(prefix, &path),
        },
        MemoriesBackendError::NotFile { path } => MemoriesBackendError::NotFile {
            path: prefixed(prefix, &path),
        },
        MemoriesBackendError::InvalidPath { path, reason } => MemoriesBackendError::InvalidPath {
            path: prefixed(prefix, &path),
            reason,
        },
        other => other,
    }
}

/// Prefix, backend, and backend-relative path picked for a routed path.
type Picked<'b, 'a> = (&'static str, &'b LocalMemoriesBackend, Option<&'a str>);

impl ScopedMemoriesBackend {
    fn pick<'a>(&self, route: &Route<'a>) -> Option<Picked<'_, 'a>> {
        match route {
            Route::Root => None,
            Route::Global(rest) => Some((GLOBAL_PREFIX, &self.global, *rest)),
            Route::Local(rest) => Some((LOCAL_PREFIX, &self.local, *rest)),
        }
    }
}

impl MemoriesBackend for ScopedMemoriesBackend {
    async fn add_ad_hoc_note(
        &self,
        request: AddAdHocMemoryNoteRequest,
    ) -> Result<AddAdHocMemoryNoteResponse, MemoriesBackendError> {
        self.local.add_ad_hoc_note(request).await
    }

    async fn list(
        &self,
        request: ListMemoriesRequest,
    ) -> Result<ListMemoriesResponse, MemoriesBackendError> {
        let route = route(request.path.as_deref())?;
        let Some((prefix, backend, rest)) = self.pick(&route) else {
            return Ok(ListMemoriesResponse {
                path: request.path,
                entries: [GLOBAL_PREFIX, LOCAL_PREFIX]
                    .into_iter()
                    .map(|path| MemoryEntry {
                        path: path.to_string(),
                        entry_type: MemoryEntryType::Directory,
                    })
                    .collect(),
                next_cursor: None,
                truncated: false,
            });
        };
        let mut response = backend
            .list(ListMemoriesRequest {
                path: rest.map(str::to_string),
                cursor: request.cursor,
                max_results: request.max_results,
            })
            .await
            .map_err(|err| prefix_error(prefix, err))?;
        response.path = request.path;
        for entry in &mut response.entries {
            entry.path = prefixed(prefix, &entry.path);
        }
        Ok(response)
    }

    async fn read(
        &self,
        request: ReadMemoryRequest,
    ) -> Result<ReadMemoryResponse, MemoriesBackendError> {
        let route = route(Some(request.path.as_str()))?;
        let Some((prefix, backend, Some(rest))) = self.pick(&route) else {
            return Err(MemoriesBackendError::NotFile { path: request.path });
        };
        let mut response = backend
            .read(ReadMemoryRequest {
                path: rest.to_string(),
                ..request.clone()
            })
            .await
            .map_err(|err| prefix_error(prefix, err))?;
        response.path = request.path;
        Ok(response)
    }

    async fn delete(
        &self,
        request: DeleteMemoryRequest,
    ) -> Result<DeleteMemoryResponse, MemoriesBackendError> {
        let route = route(Some(request.path.as_str()))?;
        let Some((prefix, backend, Some(rest))) = self.pick(&route) else {
            return Err(MemoriesBackendError::NotFile { path: request.path });
        };
        backend
            .delete(DeleteMemoryRequest {
                path: rest.to_string(),
            })
            .await
            .map_err(|err| prefix_error(prefix, err))?;
        Ok(DeleteMemoryResponse {
            path: request.path,
            deleted: true,
        })
    }

    async fn search(
        &self,
        request: SearchMemoriesRequest,
    ) -> Result<SearchMemoriesResponse, MemoriesBackendError> {
        let route = route(request.path.as_deref())?;
        if let Some((prefix, backend, rest)) = self.pick(&route) {
            let mut response = backend
                .search(SearchMemoriesRequest {
                    path: rest.map(str::to_string),
                    ..request.clone()
                })
                .await
                .map_err(|err| prefix_error(prefix, err))?;
            response.path = request.path;
            for found in &mut response.matches {
                found.path = prefixed(prefix, &found.path);
            }
            return Ok(response);
        }

        if let Some(cursor) = request.cursor {
            return Err(MemoriesBackendError::invalid_cursor(
                cursor,
                "is not supported when searching both `global/` and `local/`; search one of them",
            ));
        }
        let mut matches = Vec::new();
        let mut truncated = false;
        let mut response = None;
        for (prefix, backend) in [(GLOBAL_PREFIX, &self.global), (LOCAL_PREFIX, &self.local)] {
            let result = match backend.search(request.clone()).await {
                Ok(result) => result,
                // A scope that was never written has no root yet.
                Err(MemoriesBackendError::NotFound { .. }) => continue,
                Err(err) => return Err(prefix_error(prefix, err)),
            };
            truncated |= result.truncated;
            matches.extend(result.matches.into_iter().map(|mut found| {
                found.path = prefixed(prefix, &found.path);
                found
            }));
            response.get_or_insert((result.queries, result.match_mode));
        }
        if matches.len() > request.max_results {
            matches.truncate(request.max_results);
            truncated = true;
        }
        let (queries, match_mode) =
            response.unwrap_or((request.queries.clone(), request.match_mode.clone()));
        Ok(SearchMemoriesResponse {
            queries,
            match_mode,
            path: None,
            matches,
            next_cursor: None,
            truncated,
        })
    }
}

#[cfg(test)]
#[path = "scoped_tests.rs"]
mod tests;
