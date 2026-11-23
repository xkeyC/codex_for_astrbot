//! AstrBot: tells an open-weight model, once, that the history it continues
//! holds content of an earlier (OpenAI) model it cannot read.

use super::ContextualUserFragment;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::input_text;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OpenModelSwitchNotice {
    text: String,
}

impl OpenModelSwitchNotice {
    pub(crate) const DEFAULT_TEXT: &'static str = "Note: the model of this conversation was switched. Part of the earlier history was produced by the previous model in an encrypted form this model cannot read, so some earlier context and summaries may be missing. If you need earlier context, look it up in the message history.";

    /// The notice with `text`, or the default text.
    pub(crate) fn new(text: Option<&str>) -> Self {
        Self {
            text: text.unwrap_or(Self::DEFAULT_TEXT).to_string(),
        }
    }
}

impl ContextualUserFragment for OpenModelSwitchNotice {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("open_model.switch_notice".to_string())
    }

    fn role(&self) -> &'static str {
        "user"
    }

    fn markers(&self) -> (&'static str, &'static str) {
        Self::type_markers()
    }

    fn type_markers() -> (&'static str, &'static str) {
        ("<model_switch_notice>", "</model_switch_notice>")
    }

    fn body(&self) -> ContentItem {
        input_text(format!("\n{}\n", self.text))
    }
}
