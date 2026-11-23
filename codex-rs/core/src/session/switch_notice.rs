//! AstrBot: the one-time notice to the model that history encrypted by
//! another provider or account was left out of its requests.

use codex_history::ResponseItemEnvelope;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;

use super::session::Session;
use super::turn_context::TurnContext;
use crate::context::ContextualUserFragment;
use crate::context::EncryptedHistoryNotice;
use crate::encryption_provenance::owner;
use crate::encryption_provenance::set_owner;

/// Records the notice before the turn's input when history holds encrypted
/// content of another identity (which the requests leave out) and the latest
/// notice is not for the current identity: once per switch. The notice keeps
/// the identity it was given for (persisted, so it survives resume); a
/// compaction that drops it but keeps foreign content gives it once more.
pub(super) async fn maybe_record_switch_notice(sess: &Session, turn_context: &TurnContext) {
    let current = {
        let state = sess.state.lock().await;
        let Some(current) = state.encryption.current.clone() else {
            return;
        };
        let mut foreign = false;
        let mut latest_notice_owner = None;
        for envelope in state.history.annotated_items() {
            if is_switch_notice(&envelope.item) {
                latest_notice_owner = Some(owner(envelope).cloned());
            }
            foreign |= state.encryption.has_foreign_encrypted_content(envelope);
        }
        if !foreign || latest_notice_owner.flatten().as_ref() == Some(&current) {
            return;
        }
        current
    };
    let mut notice =
        ResponseItemEnvelope::new(ContextualUserFragment::into(EncryptedHistoryNotice::new(
            crate::provider_compaction::open_model_switch_notice(&turn_context.config),
        )));
    set_owner(&mut notice.metadata, current);
    sess.record_annotated_conversation_items(turn_context, turn_context.model_info(), vec![notice])
        .await;
}

fn is_switch_notice(item: &ResponseItem) -> bool {
    let ResponseItem::Message { role, content, .. } = item else {
        return false;
    };
    role == "user"
        && content.iter().any(|part| {
            matches!(part, ContentItem::InputText { text } if EncryptedHistoryNotice::matches_text(text))
        })
}
