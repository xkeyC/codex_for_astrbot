//! Fork addition: `delete_memory`, registered only when `memories.may_delete`
//! is `true`.
//!
//! With a memory scope the tool takes the same `global/...` / `local/...`
//! paths as the scoped read tools; `global/...` is refused unless the thread
//! may also write the shared store (`memories.may_write_global`).

use codex_extension_api::FunctionCallError;
use codex_extension_api::JsonToolOutput;
use codex_extension_api::ToolCall;
use codex_extension_api::ToolExecutor;
use codex_extension_api::ToolName;
use codex_extension_api::ToolSpec;
use codex_otel::MetricsClient;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::DELETE_TOOL_NAME;
use crate::backend::DeleteMemoryRequest;
use crate::backend::DeleteMemoryResponse;
use crate::backend::MemoriesBackend;
use crate::backend::MemoriesBackendError;
use crate::metrics::record_tool_call;
use crate::metrics::scope_from_path;
use crate::scoped::addresses_global_store;

use super::backend_error_to_function_call;
use super::memory_function_tool;
use super::memory_tool_name;
use super::parse_args;

const DESCRIPTION: &str = "Permanently delete one Codex memory file by relative path. Only use it when the user explicitly asks to delete or forget a stored memory, and delete nothing else.";
const LOCAL_ONLY_DESCRIPTION: &str = " Only this chat's `local/...` memories can be deleted; `global/...` memories are shared and must be left alone.";

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DeleteArgs {
    /// Relative path of the memory file to delete, as returned by the list,
    /// search, and read tools.
    path: String,
}

#[derive(Clone)]
pub(crate) struct DeleteMemoryTool<B> {
    pub(crate) backend: B,
    /// When `false`, `global/...` paths are refused before reaching the backend.
    pub(crate) may_delete_global: bool,
    pub(crate) metrics_client: Option<MetricsClient>,
}

impl<'call, B> ToolExecutor<ToolCall<'call>> for DeleteMemoryTool<B>
where
    B: MemoriesBackend,
{
    fn tool_name(&self) -> ToolName {
        memory_tool_name(DELETE_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        let description = if self.may_delete_global {
            DESCRIPTION.to_string()
        } else {
            format!("{DESCRIPTION}{LOCAL_ONLY_DESCRIPTION}")
        };
        memory_function_tool::<DeleteArgs, DeleteMemoryResponse>(DELETE_TOOL_NAME, &description)
    }

    fn handle<'a>(&'a self, call: ToolCall<'call>) -> codex_extension_api::ToolExecutorFuture<'a>
    where
        'call: 'a,
    {
        Box::pin(self.handle_call(call))
    }
}

impl<B> DeleteMemoryTool<B>
where
    B: MemoriesBackend,
{
    async fn handle_call(
        &self,
        call: ToolCall<'_>,
    ) -> Result<Box<dyn codex_extension_api::ToolOutput>, FunctionCallError> {
        let args: DeleteArgs = parse_args(&call)?;
        let path = args.path;
        let scope = scope_from_path(path.as_str());
        let response = if self.may_delete_global || !addresses_global_store(path.as_str()) {
            self.backend
                .delete(DeleteMemoryRequest { path: path.clone() })
                .await
        } else {
            Err(MemoriesBackendError::invalid_path(
                path,
                "is a shared memory; this chat may only delete its own `local/` memories",
            ))
        };
        record_tool_call(
            self.metrics_client.as_ref(),
            DELETE_TOOL_NAME,
            scope,
            response.is_ok(),
            "not_applicable",
        );
        let response = response.map_err(backend_error_to_function_call)?;
        Ok(Box::new(JsonToolOutput::new(json!(response))))
    }
}
