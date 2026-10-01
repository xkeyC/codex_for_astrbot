//! AstrBot: the end of a local-infra voice conversation, recorded when it
//! happens.

use super::session::Session;
use codex_history::RolloutItem;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::debug;
use tracing::warn;

impl Session {
    /// Records that the realtime conversation ended: its end instructions
    /// and the inactive realtime state. A hung-up voice thread runs no more
    /// turns (and the host may unload it), so without this the next call's
    /// first turn would find the conversation still active and tell neither
    /// the end nor the new start (with that call's instructions).
    ///
    /// Not while a turn runs (the voice turn was stopped at the hang-up; any
    /// other records the change itself), nor without a context baseline (after
    /// a compaction: the next turn injects the whole context anyway).
    pub(crate) async fn record_local_voice_end(self: &Arc<Self>) {
        if self.active_turn.lock().await.is_some() {
            debug!("local-infra conversation: a turn runs; it records the end");
            return;
        }
        {
            let state = self.state.lock().await;
            if state.reference_context_item().is_none()
                || state.history.world_state_baseline().is_none()
            {
                return;
            }
        }
        let turn = self.new_default_turn().await;
        let step = match self
            .capture_step_context(Arc::clone(&turn), &CancellationToken::new())
            .await
        {
            Ok(step) => step,
            Err(err) => {
                warn!("local-infra conversation: cannot record its end: {err}");
                return;
            }
        };
        // A call started meanwhile: the thread is in a call again.
        if step.realtime.active {
            return;
        }
        let world_state = match self.build_world_state_for_step(&step).await {
            Ok(world_state) => world_state,
            Err(err) => {
                warn!("local-infra conversation: cannot record its end: {err}");
                return;
            }
        };
        let (fragments, world_state_item) = self
            .state
            .lock()
            .await
            .history
            .update_world_state(&world_state);
        let items = crate::context_manager::updates::merge_contextual_fragments(fragments);
        if !items.is_empty() {
            self.record_conversation_items(&turn, &step.settings.model_info, &items)
                .await;
        }
        if let Some(world_state_item) = world_state_item {
            self.persist_rollout_items(&[RolloutItem::WorldState(world_state_item)])
                .await;
        }
    }
}
