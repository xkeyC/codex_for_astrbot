//! Fork addition: per-provider options, `[model_provider_options.<id>]`.
//!
//! Third-party providers are often served models Codex has no metadata for
//! (it then falls back to generic defaults and warns), and Codex decides on
//! its own whether a provider runs remote compaction. These options let the
//! host say both for a provider id of `model_providers`:
//!
//! ```toml
//! [model_provider_options.deepseek]
//! compaction = "local"
//! wire = "chat"
//! extra_body = { thinking = { type = "disabled" } }
//! extra_body_remove = ["reasoning_effort"]
//! omit_turn_metadata = true
//! files_api = { expires_seconds = 86400 }
//!
//! [model_provider_options.deepseek.models."deepseek-v4.1-flash"]
//! context_window = 128000
//! default_reasoning_level = "none"
//! input_modalities = ["text", "image"]
//! ```
//!
//! A model's table is merged, field by field, over the metadata Codex would
//! otherwise use (its catalog entry, or the fallback); the model then counts
//! as known. Nothing here changes a provider or model it does not name.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;

/// Options for one provider id.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct ModelProviderOptions {
    /// How the provider's threads compact their history.
    #[serde(default)]
    pub compaction: ProviderCompaction,
    /// Which API the provider's requests go to.
    #[serde(default)]
    pub wire: ProviderWire,
    /// Chat wire: merged over each request body's top level (a provider's
    /// own fields).
    #[serde(default)]
    #[schemars(with = "BTreeMap<String, serde_json::Value>")]
    pub extra_body: serde_json::Map<String, serde_json::Value>,
    /// Chat wire: top-level fields Codex sends that the provider rejects
    /// (`reasoning_effort`, say), left out of each request body.
    #[serde(default)]
    pub extra_body_remove: Vec<String>,
    /// Leave the `x-codex-turn-metadata` header (and its copy in the body's
    /// `client_metadata`) out of the provider's requests. Some providers take
    /// a request carrying it as Codex's and change course: DeepSeek then
    /// thinks whatever the reasoning effort.
    #[serde(default)]
    pub omit_turn_metadata: bool,
    /// Upload the requests' inline images to the provider's Files API
    /// (`POST /files`, purpose `user_data`: DeepSeek's, OpenAI's shape) and
    /// reference them by file id. Codex sends the whole history with every
    /// request, so an image a tool returned is otherwise sent again with
    /// each one; uploaded, it is sent once and its reference stays the same
    /// (the provider's prefix cache still matches).
    #[serde(default)]
    pub files_api: Option<FilesApiOptions>,
    /// Model metadata by model slug: any fields of Codex's model info (for
    /// example `context_window`, `auto_compact_token_limit`,
    /// `supported_reasoning_levels` as `[{effort, description}]`,
    /// `default_reasoning_level`, `input_modalities`), merged field by field
    /// over what Codex would use otherwise; a field that does not fit is
    /// skipped (and logged).
    #[serde(default)]
    #[schemars(with = "BTreeMap<String, BTreeMap<String, serde_json::Value>>")]
    pub models: BTreeMap<String, serde_json::Value>,
}

/// How images go to a provider's Files API.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct FilesApiOptions {
    /// The provider deletes an upload this long after it (DeepSeek takes
    /// 3600 to 2592000); one is uploaded again an hour before that, should
    /// the conversation still use it.
    #[serde(default = "default_files_expires_seconds")]
    pub expires_seconds: u64,
}

impl Default for FilesApiOptions {
    fn default() -> Self {
        Self {
            expires_seconds: default_files_expires_seconds(),
        }
    }
}

fn default_files_expires_seconds() -> u64 {
    86_400
}

/// The API a provider's requests go to.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ProviderWire {
    /// The provider's `wire_api` (Responses).
    #[default]
    Responses,
    /// `/chat/completions`: for a provider whose Responses endpoint is
    /// missing or broken. Hosted tools (web search) and remote compaction
    /// are not available over it.
    Chat,
}

/// Where a provider's history compaction runs.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ProviderCompaction {
    /// Codex decides (remote for OpenAI and Azure, local otherwise).
    #[default]
    Auto,
    /// The model summarizes the history itself, in an ordinary request.
    Local,
    /// The provider's `/responses/compact` endpoint (it must have one).
    Remote,
}
