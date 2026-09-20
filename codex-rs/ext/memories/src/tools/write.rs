//! Fork addition: `memories.write`, registered only for a consolidation agent
//! (`memories.maintenance_tools`).
//!
//! Consolidation's whole job is rewriting the files under the memory root. It
//! normally does that with a shell, but a host that keeps Codex off its
//! filesystem gives it no execution environment at all, and then neither the
//! shell nor `apply_patch` is registered. This tool is the way back in: plain
//! file writes, confined to the one root the agent was pointed at.

use codex_extension_api::JsonToolOutput;
use codex_extension_api::ToolCall;
use codex_extension_api::ToolExecutor;
use codex_extension_api::ToolName;
use codex_extension_api::ToolSpec;
use codex_otel::MetricsClient;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::WRITE_TOOL_NAME;
use crate::backend::MemoriesBackend;
use crate::backend::WriteMemoryRequest;
use crate::backend::WriteMemoryResponse;
use crate::metrics::record_tool_call;
use crate::metrics::scope_from_path;

use super::backend_error_to_function_call;
use super::memory_function_tool;
use super::memory_tool_name;
use super::parse_args;

const DESCRIPTION: &str = "Create or overwrite one memory file with the given content. The path is relative to the memory root and the whole file is replaced, so include everything the file should keep.";

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    /// Relative path of the memory file to write, as returned by the list and
    /// read tools. Missing parent directories are created.
    path: String,
    /// Full new contents of the file.
    content: String,
}

#[derive(Clone)]
pub(super) struct WriteTool<B> {
    pub(super) backend: B,
    pub(super) metrics_client: Option<MetricsClient>,
}

impl<'call, B> ToolExecutor<ToolCall<'call>> for WriteTool<B>
where
    B: MemoriesBackend,
{
    fn tool_name(&self) -> ToolName {
        memory_tool_name(WRITE_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        memory_function_tool::<WriteArgs, WriteMemoryResponse>(WRITE_TOOL_NAME, DESCRIPTION)
    }

    fn handle<'a>(&'a self, call: ToolCall<'call>) -> codex_extension_api::ToolExecutorFuture<'a>
    where
        'call: 'a,
    {
        Box::pin(self.handle_call(call))
    }
}

impl<B> WriteTool<B>
where
    B: MemoriesBackend,
{
    async fn handle_call(
        &self,
        call: ToolCall<'_>,
    ) -> Result<Box<dyn codex_extension_api::ToolOutput>, codex_extension_api::FunctionCallError>
    {
        let args: WriteArgs = parse_args(&call)?;
        let scope = scope_from_path(args.path.as_str());
        let response = self
            .backend
            .write(WriteMemoryRequest {
                path: args.path,
                content: args.content,
            })
            .await;
        record_tool_call(
            self.metrics_client.as_ref(),
            WRITE_TOOL_NAME,
            scope,
            response.is_ok(),
            "not_applicable",
        );
        let response = response.map_err(backend_error_to_function_call)?;
        Ok(Box::new(JsonToolOutput::new(json!(response))))
    }
}
