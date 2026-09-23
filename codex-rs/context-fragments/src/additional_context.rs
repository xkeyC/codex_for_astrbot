use codex_protocol::models::ContentItemKind;
use codex_utils_string::truncate_middle_with_token_budget;

use crate::ContextualUserFragment;

pub const MAX_ADDITIONAL_CONTEXT_VALUE_TOKENS: usize = 1_000;
const ADDITIONAL_CONTEXT_END_MARKER_SUFFIX: &str = ">";
const ADDITIONAL_CONTEXT_START_MARKER_PREFIX: &str = "<external_";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdditionalContextUserFragment {
    key: String,
    value: String,
    max_tokens: usize,
}

impl AdditionalContextUserFragment {
    pub fn new(key: String, value: String) -> Self {
        Self {
            key,
            value,
            max_tokens: MAX_ADDITIONAL_CONTEXT_VALUE_TOKENS,
        }
    }

    /// Fork addition: a token budget other than the default for the value.
    pub fn with_max_tokens(mut self, max_tokens: usize) -> Self {
        self.max_tokens = max_tokens;
        self
    }
}

impl ContextualUserFragment for AdditionalContextUserFragment {
    fn role(&self) -> &'static str {
        "user"
    }

    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind(format!("additional_content.{}", self.key))
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        (
            ADDITIONAL_CONTEXT_START_MARKER_PREFIX,
            ADDITIONAL_CONTEXT_END_MARKER_SUFFIX,
        )
    }

    fn matches_text(text: &str) -> bool {
        let trimmed = text.trim();
        let Some(rest) = trimmed.strip_prefix(ADDITIONAL_CONTEXT_START_MARKER_PREFIX) else {
            return false;
        };
        let Some((key, value_and_close)) = rest.split_once(ADDITIONAL_CONTEXT_END_MARKER_SUFFIX)
        else {
            return false;
        };

        value_and_close.ends_with(&format!("</external_{key}>"))
    }

    fn body(&self) -> String {
        additional_context_body(&self.key, &self.value, self.max_tokens)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdditionalContextDeveloperFragment {
    key: String,
    value: String,
    max_tokens: usize,
}

impl AdditionalContextDeveloperFragment {
    pub fn new(key: String, value: String) -> Self {
        Self {
            key,
            value,
            max_tokens: MAX_ADDITIONAL_CONTEXT_VALUE_TOKENS,
        }
    }

    /// Fork addition: a token budget other than the default for the value.
    pub fn with_max_tokens(mut self, max_tokens: usize) -> Self {
        self.max_tokens = max_tokens;
        self
    }
}

impl ContextualUserFragment for AdditionalContextDeveloperFragment {
    fn role(&self) -> &'static str {
        "developer"
    }

    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind(format!("additional_content.{}", self.key))
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("", "")
    }

    fn body(&self) -> String {
        additional_context_developer_body(&self.key, &self.value, self.max_tokens)
    }
}

fn additional_context_body(key: &str, value: &str, max_tokens: usize) -> String {
    let value = truncate_middle_with_token_budget(value, max_tokens).0;
    format!("{key}>{value}</external_{key}")
}

fn additional_context_developer_body(key: &str, value: &str, max_tokens: usize) -> String {
    let value = truncate_middle_with_token_budget(value, max_tokens).0;
    format!("<{key}>{value}</{key}>")
}
