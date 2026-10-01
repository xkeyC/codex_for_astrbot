//! Fork addition: realtime voice over local-multimodal-infra.
//!
//! With `[realtime] backend = "local_multimodal_infra"` a thread's realtime
//! conversation does not go to an OpenAI realtime model: Codex opens the
//! `/v1/realtime` WebSocket of a local-multimodal-infra server in audio mode
//! (it listens and speaks: VAD, ASR, TTS, barge-in) and answers what it hears
//! with ordinary turns of the thread, on the thread's own provider and model.
//!
//! ```toml
//! [realtime]
//! backend = "local_multimodal_infra"
//!
//! [realtime.local_infra]
//! url = "ws://127.0.0.1:17890/v1/realtime"
//! token = "..."
//! ref_audio_path = "/data/voice.wav"
//! ref_text = "What the reference recording says."
//! idle_compact_percent = 70
//!
//! [realtime.local_infra.session]
//! name = "Xiaole"
//! group = false
//! tts_emotion = "calm"
//! ```

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;

/// Which service carries a realtime conversation.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeBackend {
    /// An OpenAI realtime model (upstream behavior).
    #[default]
    Openai,
    /// A local-multimodal-infra server for the audio; the thread's own model
    /// for the words.
    LocalMultimodalInfra,
}

/// `[realtime.local_infra]`: the server and how its sessions start.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema)]
#[schemars(deny_unknown_fields)]
pub struct LocalInfraRealtimeConfig {
    /// The server's realtime WebSocket, e.g. `ws://127.0.0.1:17890/v1/realtime`.
    pub url: Option<String>,
    /// Bearer token (one of the server's `LOCAL_MCP_INFER_TOKENS`), if it
    /// asks for one.
    pub token: Option<String>,
    /// A WAV file whose voice the bot speaks with (sent as `ref_audio`);
    /// without it the server's default voice.
    pub ref_audio_path: Option<String>,
    /// What `ref_audio_path` says (sent as `ref_text`, only with it): a TTS
    /// model that takes it (Qwen3-TTS) clones the voice in context, closer
    /// than from the recording alone.
    pub ref_text: Option<String>,
    /// More `session.start` config fields, sent as they are (`name`,
    /// `aliases`, `group`, `tts_emotion`, `min_silence_ms`, ...). `mode` is
    /// always `audio`.
    #[serde(default)]
    #[schemars(with = "BTreeMap<String, serde_json::Value>")]
    pub session: BTreeMap<String, serde_json::Value>,
    /// While the conversation is idle, compact the thread's history once its
    /// prompt passes this percentage of the model's context window, so no
    /// turn waits for it (0 or unset: only Codex's own compaction). After one
    /// the prompt must grow by a tenth of the window before the next.
    pub idle_compact_percent: Option<u8>,
}
