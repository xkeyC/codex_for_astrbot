//! Realtime (voice) conversation on a loaded thread.
//!
//! The realtime model listens and speaks; tasks are handed off to the thread
//! it is attached to, and its events arrive through the thread's normal event
//! stream (`realtime_conversation_started`, `realtime_conversation_realtime`,
//! `realtime_conversation_sdp`, `realtime_conversation_closed`).
//!
//! With the WebRTC transport the media stays in the host: it sends an SDP
//! offer here and gets the answer back as a `realtime_conversation_sdp` event.
//! Core only does the signalling and the sideband control channel, which works
//! with a ChatGPT account (the websocket transport needs an API key).

use std::collections::BTreeMap;

use anyhow::Result;
use codex_protocol::protocol::CodexResponseHandoffMode;
use codex_protocol::protocol::ConversationAudioParams;
use codex_protocol::protocol::ConversationSpeechParams;
use codex_protocol::protocol::ConversationStartParams;
use codex_protocol::protocol::ConversationStartTransport;
use codex_protocol::protocol::ConversationTextParams;
use codex_protocol::protocol::ConversationTextRole;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::RealtimeAudioFrame;
use codex_protocol::protocol::RealtimeConversationVersion;
use codex_protocol::protocol::RealtimeOutputModality;
use codex_protocol::protocol::RealtimeVoice;
use codex_protocol::protocol::RealtimeVoicesList;
use serde::Deserialize;
use serde::Deserializer;

use crate::engine::Engine;

#[derive(Debug, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RealtimeTransport {
    /// Media over a WebRTC call the host terminates; `sdp` is its offer.
    Webrtc { sdp: String },
    /// Audio as base64 PCM through core. Needs API key auth.
    Websocket {},
    /// Attach the sideband to a call the host created itself.
    ExistingCall { call_id: String },
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RealtimeTextItem {
    pub text: String,
    #[serde(default)]
    pub role: ConversationTextRole,
}

/// `realtime_start` request. Omitted fields take the app server's defaults.
///
/// Keys are snake_case; `codex_response_handoff_mode` values are camelCase
/// (`thinking`, `commentary`, `bemTags`) as in the app-server API.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RealtimeStartRequest {
    /// `None` uses the transport from `[realtime]` config.
    #[serde(default)]
    pub transport: Option<RealtimeTransport>,
    #[serde(default = "default_output_modality")]
    pub output_modality: RealtimeOutputModality,
    /// Host decides what gets spoken from Codex output
    /// (`realtime_append_speech`) instead of core feeding it back.
    #[serde(default)]
    pub client_managed_handoffs: bool,
    /// Defaults to true, except when attaching to an existing call.
    #[serde(default)]
    pub include_startup_context: Option<bool>,
    #[serde(default)]
    pub initial_items: Vec<RealtimeTextItem>,
    /// Absent keeps the default prompt; `null` sends none.
    #[serde(default, deserialize_with = "present_value")]
    pub prompt: Option<Option<String>>,
    #[serde(default)]
    pub realtime_start_instructions: Option<String>,
    #[serde(default)]
    pub realtime_end_instructions: Option<String>,
    #[serde(default)]
    pub realtime_session_id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub version: Option<RealtimeConversationVersion>,
    #[serde(default)]
    pub voice: Option<RealtimeVoice>,
    #[serde(default)]
    pub delegation_ack_filler: Option<bool>,
    #[serde(default)]
    pub flush_transcript_tail_on_session_end: bool,
    #[serde(default)]
    pub codex_responses_as_items: bool,
    #[serde(default)]
    pub codex_response_item_prefix: Option<String>,
    #[serde(default)]
    pub codex_response_handoff_mode: CodexResponseHandoffMode,
    #[serde(default)]
    pub codex_response_handoff_channel_prefixes: Option<BTreeMap<String, Vec<String>>>,
}

fn default_output_modality() -> RealtimeOutputModality {
    RealtimeOutputModality::Audio
}

/// Tells a present `null` (`Some(None)`) apart from an absent key (`None`).
fn present_value<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

impl RealtimeStartRequest {
    pub fn into_params(self) -> ConversationStartParams {
        let attaches_existing_call =
            matches!(self.transport, Some(RealtimeTransport::ExistingCall { .. }));
        ConversationStartParams {
            client_managed_handoffs: self.client_managed_handoffs,
            delegation_ack_filler: self.delegation_ack_filler,
            flush_transcript_tail_on_session_end: self.flush_transcript_tail_on_session_end,
            codex_responses_as_items: self.codex_responses_as_items,
            codex_response_item_prefix: self.codex_response_item_prefix,
            codex_response_handoff_mode: self.codex_response_handoff_mode,
            codex_response_handoff_channel_prefixes: self.codex_response_handoff_channel_prefixes,
            model: self.model,
            output_modality: self.output_modality,
            include_startup_context: self
                .include_startup_context
                .unwrap_or(!attaches_existing_call),
            initial_items: self
                .initial_items
                .into_iter()
                .map(|item| ConversationTextParams {
                    text: item.text,
                    role: item.role,
                })
                .collect(),
            realtime_start_instructions: self.realtime_start_instructions,
            realtime_end_instructions: self.realtime_end_instructions,
            prompt: self.prompt,
            realtime_session_id: self.realtime_session_id,
            transport: self.transport.map(|transport| match transport {
                RealtimeTransport::Webrtc { sdp } => ConversationStartTransport::Webrtc { sdp },
                RealtimeTransport::Websocket {} => ConversationStartTransport::Websocket,
                RealtimeTransport::ExistingCall { call_id } => {
                    ConversationStartTransport::ExistingCall {
                        call_id,
                        sideband_base_url: None,
                    }
                }
            }),
            version: self.version,
            voice: self.voice,
        }
    }
}

/// Parses `"user" | "developer" | "assistant"`.
pub fn parse_text_role(role: &str) -> Result<ConversationTextRole> {
    Ok(serde_json::from_value(serde_json::Value::from(role))?)
}

/// The voices each realtime version accepts; static, so no thread is needed.
pub fn list_voices() -> RealtimeVoicesList {
    RealtimeVoicesList::builtin()
}

impl Engine {
    /// Starts a realtime conversation on the thread. The outcome arrives as
    /// events: `realtime_conversation_started`, then `realtime_conversation_sdp`
    /// (the WebRTC answer). A failed start sends only a
    /// `realtime_conversation_realtime` event whose payload is `Error`; treat
    /// that as the end of the start. ChatGPT (subscription) WebRTC calls need
    /// `version: "v3"`: without a version core picks v1, which they reject.
    pub async fn realtime_start(
        &self,
        thread_id: &str,
        request: RealtimeStartRequest,
    ) -> Result<()> {
        self.submit_realtime(
            thread_id,
            Op::RealtimeConversationStart(request.into_params()),
        )
        .await
    }

    /// Text into the realtime conversation, as if said by `role`.
    pub async fn realtime_append_text(
        &self,
        thread_id: &str,
        text: String,
        role: ConversationTextRole,
    ) -> Result<()> {
        self.submit_realtime(
            thread_id,
            Op::RealtimeConversationText(ConversationTextParams { text, role }),
        )
        .await
    }

    /// Text for the realtime model to speak (client-managed handoffs).
    pub async fn realtime_append_speech(&self, thread_id: &str, text: String) -> Result<()> {
        self.submit_realtime(
            thread_id,
            Op::RealtimeConversationSpeech(ConversationSpeechParams { text }),
        )
        .await
    }

    /// Audio input; only used by the websocket transport.
    pub async fn realtime_append_audio(
        &self,
        thread_id: &str,
        frame: RealtimeAudioFrame,
    ) -> Result<()> {
        self.submit_realtime(
            thread_id,
            Op::RealtimeConversationAudio(ConversationAudioParams { frame }),
        )
        .await
    }

    pub async fn realtime_stop(&self, thread_id: &str) -> Result<()> {
        self.submit_realtime(thread_id, Op::RealtimeConversationClose)
            .await
    }

    async fn submit_realtime(&self, thread_id: &str, op: Op) -> Result<()> {
        self.thread(thread_id).await?.submit(op).await?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "realtime_tests.rs"]
mod tests;
