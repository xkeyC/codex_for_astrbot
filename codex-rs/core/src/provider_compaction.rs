//! AstrBot: `model_provider_options.<id>.compaction` over the provider's own
//! remote compaction support (see `codex_config::model_provider_options`).

use crate::session::turn_context::TurnContext;
use codex_config::model_provider_options::ProviderCompaction;
use codex_model_provider::RemoteCompactionSupport;

/// How this turn's provider compacts: as configured for it, else as the
/// provider itself says.
pub(crate) fn remote_compaction_support(turn_context: &TurnContext) -> RemoteCompactionSupport {
    let options = turn_context
        .config
        .model_provider_options
        .get(&turn_context.config.model_provider_id);
    match options.map(|options| options.compaction) {
        Some(ProviderCompaction::Local) => RemoteCompactionSupport::Unsupported,
        Some(ProviderCompaction::Remote) => RemoteCompactionSupport::V2,
        Some(ProviderCompaction::Auto) | None => {
            turn_context.provider.capabilities().remote_compaction
        }
    }
}
