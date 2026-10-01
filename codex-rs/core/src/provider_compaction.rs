//! AstrBot: `model_provider_options.<id>.compaction` over the provider's own
//! remote compaction support, and the provider's wire (see
//! `codex_config::model_provider_options`).

use crate::config::Config;
use crate::session::turn_context::TurnContext;
use codex_api::ChatOptions;
use codex_config::model_provider_options::ProviderCompaction;
use codex_config::model_provider_options::ProviderWire;
use codex_model_provider::RemoteCompactionSupport;

/// How this turn's provider compacts: as configured for it, else as the
/// provider itself says.
pub(crate) fn remote_compaction_support(turn_context: &TurnContext) -> RemoteCompactionSupport {
    let options = turn_context
        .config
        .model_provider_options
        .get(&turn_context.config.model_provider_id);
    // The chat wire has no remote compaction.
    if options.is_some_and(|options| options.wire == ProviderWire::Chat) {
        return RemoteCompactionSupport::Unsupported;
    }
    match options.map(|options| options.compaction) {
        Some(ProviderCompaction::Local) => RemoteCompactionSupport::Unsupported,
        Some(ProviderCompaction::Remote) => RemoteCompactionSupport::V2,
        Some(ProviderCompaction::Auto) | None => {
            turn_context.provider.capabilities().remote_compaction
        }
    }
}

/// How requests to the configured provider go to `/chat/completions`, when
/// its wire is chat.
pub fn chat_wire_options(config: &Config) -> Option<ChatOptions> {
    let options = config
        .model_provider_options
        .get(&config.model_provider_id)?;
    (options.wire == ProviderWire::Chat).then(|| ChatOptions {
        extra_body: options.extra_body.clone(),
        remove: options.extra_body_remove.clone(),
    })
}
