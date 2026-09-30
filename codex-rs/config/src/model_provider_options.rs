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
    /// Model metadata by model slug: any fields of Codex's model info (for
    /// example `context_window`, `auto_compact_token_limit`,
    /// `supported_reasoning_levels`, `default_reasoning_level`,
    /// `input_modalities`), merged over what Codex would use otherwise.
    #[serde(default)]
    #[schemars(with = "BTreeMap<String, BTreeMap<String, serde_json::Value>>")]
    pub models: BTreeMap<String, serde_json::Value>,
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
