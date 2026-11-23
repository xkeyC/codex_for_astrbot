//! Typed reasoning settings for a durable Responses API configuration update.

use crate::TS;
use crate::openai_models::ReasoningEffort;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

/// Reasoning settings interpreted by the backend for the routed model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, JsonSchema, TS)]
pub struct ConfigurationReasoning {
    pub effort: ReasoningEffort,
}

#[cfg(test)]
#[path = "configuration_update_tests.rs"]
mod tests;
