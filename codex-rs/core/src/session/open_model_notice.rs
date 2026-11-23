//! AstrBot: the one-time notice to an open-weight model that continues a
//! thread an OpenAI model left encrypted content in.

use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;

use super::session::Session;
use super::turn_context::TurnContext;
use crate::context::ContextualUserFragment;
use crate::context::OpenModelSwitchNotice;
use crate::open_model::has_openai_encrypted_content;

/// Records the notice before the turn's input when the provider serves an
/// open model, the history holds OpenAI-encrypted content and no notice yet.
/// The notice stays in history (and in the rollout), so it is given once;
/// once more after a compaction that kept encrypted content but not it.
pub(super) async fn maybe_record_open_model_switch_notice(
    sess: &Session,
    turn_context: &TurnContext,
) {
    if !crate::provider_compaction::open_model(&turn_context.config) {
        return;
    }
    let notice_due = {
        let state = sess.state.lock().await;
        let mut encrypted = false;
        for item in state.history.raw_items() {
            if is_switch_notice(item) {
                return;
            }
            encrypted |= has_openai_encrypted_content(item);
        }
        encrypted
    };
    if !notice_due {
        return;
    }
    let notice = ContextualUserFragment::into(OpenModelSwitchNotice::new(
        crate::provider_compaction::open_model_switch_notice(&turn_context.config),
    ));
    sess.record_conversation_items(
        turn_context,
        turn_context.model_info(),
        std::slice::from_ref(&notice),
    )
    .await;
}

fn is_switch_notice(item: &ResponseItem) -> bool {
    let ResponseItem::Message { role, content, .. } = item else {
        return false;
    };
    role == "user"
        && content.iter().any(|part| {
            matches!(part, ContentItem::InputText { text } if OpenModelSwitchNotice::matches_text(text))
        })
}
