//! AstrBot: tells the model, once per provider or account switch, that the
//! history it continues held content another identity encrypted, which its
//! requests leave out.

use super::ContextualUserFragment;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::input_text;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct EncryptedHistoryNotice {
    text: String,
}

impl EncryptedHistoryNotice {
    pub(crate) const DEFAULT_TEXT: &'static str = "Note: the model provider or account of this conversation changed. Part of the earlier history was produced under the previous one in an encrypted form that cannot be read now, so some earlier context and summaries may be missing. If you need earlier context, look it up in the message history.";

    /// The notice with `text`, or the default text.
    pub(crate) fn new(text: Option<&str>) -> Self {
        Self {
            text: text.unwrap_or(Self::DEFAULT_TEXT).to_string(),
        }
    }
}

impl ContextualUserFragment for EncryptedHistoryNotice {
    fn content_kind(&self) -> ContentItemKind {
        ContentItemKind("encrypted_history.switch_notice".to_string())
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
