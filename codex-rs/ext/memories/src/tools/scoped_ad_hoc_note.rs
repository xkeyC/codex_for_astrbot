//! Fork addition: `add_ad_hoc_note` for threads with a memory scope that may
//! also write to the global store. Notes go to the chat-private scope unless
//! the model explicitly asks for `scope: "global"`.

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

use crate::ADD_AD_HOC_NOTE_TOOL_NAME;
use crate::backend::AddAdHocMemoryNoteRequest;
use crate::backend::AddAdHocMemoryNoteResponse;
use crate::backend::MemoriesBackend;
use crate::metrics::record_tool_call;

use super::backend_error_to_function_call;
use super::memory_function_tool;
use super::memory_tool_name;
use super::parse_args;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum NoteScope {
    /// Private to the current chat (default).
    #[default]
    Local,
    /// Shared with every chat. Only for impersonal, general knowledge.
    Global,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ScopedAddAdHocNoteArgs {
    /// Name of the note file to create, in
    /// YYYY-MM-DDTHH-MM-SS-<slug>.md format. The slug must use only lowercase
    /// ASCII letters, digits, and hyphens.
    #[schemars(
        length(min = 24, max = 128),
        regex(pattern = r"^\d{4}-\d{2}-\d{2}T\d{2}-\d{2}-\d{2}-[a-z0-9][a-z0-9-]{0,79}\.md$")
    )]
    filename: String,
    /// Verbatim Markdown note to append to the ad-hoc memory notes.
    #[schemars(length(min = 1))]
    note: String,
    /// Where to store the note: "local" (this chat only, default) or "global"
    /// (shared with every chat). Never store information about specific
    /// people, users, or groups globally.
    #[serde(default)]
    scope: NoteScope,
}

#[derive(Clone)]
pub(crate) struct ScopedAddAdHocNoteTool<B> {
    pub(crate) local: B,
    pub(crate) global: B,
    pub(crate) metrics_client: Option<MetricsClient>,
}

impl<'call, B> ToolExecutor<ToolCall<'call>> for ScopedAddAdHocNoteTool<B>
where
    B: MemoriesBackend,
{
    fn tool_name(&self) -> ToolName {
        memory_tool_name(ADD_AD_HOC_NOTE_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        memory_function_tool::<ScopedAddAdHocNoteArgs, AddAdHocMemoryNoteResponse>(
            ADD_AD_HOC_NOTE_TOOL_NAME,
            "Create one append-only ad-hoc memory note after the user explicitly asks Codex to remember, forget, or update something. Notes are private to this chat unless scope is \"global\".",
        )
    }

    fn handle<'a>(&'a self, call: ToolCall<'call>) -> codex_extension_api::ToolExecutorFuture<'a>
    where
        'call: 'a,
    {
        Box::pin(self.handle_call(call))
    }
}

impl<B> ScopedAddAdHocNoteTool<B>
where
    B: MemoriesBackend,
{
    async fn handle_call(
        &self,
        call: ToolCall<'_>,
    ) -> Result<Box<dyn codex_extension_api::ToolOutput>, FunctionCallError> {
        let args: ScopedAddAdHocNoteArgs = parse_args(&call)?;
        let backend = match args.scope {
            NoteScope::Local => self.local.clone(),
            NoteScope::Global => self.global.clone(),
        };
        let response = backend
            .add_ad_hoc_note(AddAdHocMemoryNoteRequest {
                filename: args.filename,
                note: args.note,
            })
            .await;
        record_tool_call(
            self.metrics_client.as_ref(),
            ADD_AD_HOC_NOTE_TOOL_NAME,
            "ad_hoc_notes",
            response.is_ok(),
            "not_applicable",
        );
        let response = response.map_err(backend_error_to_function_call)?;
        Ok(Box::new(JsonToolOutput::new(json!(response))))
    }
}
