use codex_protocol::config_types::Personality;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::openai_models::ToolMode;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default)]
pub struct ModelsManagerConfig {
    pub model_context_window: Option<i64>,
    pub model_auto_compact_token_limit: Option<i64>,
    pub tool_output_token_limit: Option<usize>,
    pub base_instructions: Option<String>,
    pub personality: Option<Personality>,
    pub model_catalog: Option<ModelsResponse>,
    /// Forces the tool mode regardless of the model catalog.
    pub tool_mode: Option<ToolMode>,
    /// AstrBot: model metadata of the provider by model slug (partial model
    /// info objects), merged over the catalog entry or the fallback.
    pub model_overrides: BTreeMap<String, serde_json::Value>,
}
