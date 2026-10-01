//! AstrBot: realtime voice over local-multimodal-infra
//! (`[realtime] backend = "local_multimodal_infra"`, see
//! `codex_config::realtime_local_infra`).
//!
//! The server (its `/v1/realtime` WebSocket, in audio mode) listens and
//! speaks: VAD, recognition, joining an utterance to the one it continues,
//! barge-in, TTS. This thread does the talking: an utterance that wants a
//! reply starts a turn (stopping one still running for an earlier utterance),
//! and the text of the turn's messages is streamed back to be spoken. A
//! message that is only `SILENCE_MARKER` says nothing.
//!
//! What the host appends (`realtime_append_text` / `realtime_append_speech`:
//! a task's result, something to say first) is told in a turn of its own once
//! nobody talks and the bot is quiet, or after `RELAY_WAIT` at the latest.
//! Talk that wants no reply (in a group, others talking while the bot speaks)
//! goes with the next input as context, as does what the listener actually
//! heard of a reply that was cut off.
//!
//! The thread's history only ever grows (its prefix stays cacheable); while
//! the conversation is idle and the prompt has grown past
//! `idle_compact_percent` of the context window, the history is compacted,
//! so no reply waits for it.

use super::*;
use crate::tasks::CompactTask;
use codex_config::realtime_local_infra::LocalInfraRealtimeConfig;
use codex_protocol::items::AgentMessageItem;
use codex_protocol::items::TurnItem;
use codex_protocol::protocol::RealtimeConversationVersion;
use codex_protocol::protocol::RealtimeInputAudioSpeechStarted;
use codex_protocol::protocol::RealtimeResponseCancelled;
use codex_protocol::protocol::RealtimeResponseDone;
use codex_protocol::protocol::RealtimeTranscriptDelta;
use codex_protocol::protocol::RealtimeTranscriptDone;
use codex_protocol::protocol::TurnAbortReason;
use futures::SinkExt;
use futures::StreamExt;
use std::sync::Weak;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

/// What the model says instead of an answer to stay silent.
pub(crate) const SILENCE_MARKER: &str = "<silence>";
/// The server's input and output sample rates (16-bit mono PCM).
const INPUT_RATE: u32 = 16_000;
const OUTPUT_RATE: u32 = 24_000;
/// First use loads the server's models (TTS takes tens of seconds).
const START_TIMEOUT: Duration = Duration::from_secs(300);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Appended words wait at most this long for a quiet moment.
const RELAY_WAIT: Duration = Duration::from_secs(20);
/// A relay that could not be submitted is tried again after this.
const RELAY_RETRY: Duration = Duration::from_secs(2);
/// Idle this long before the history is compacted.
const IDLE_BEFORE_COMPACT: Duration = Duration::from_secs(2);
const TICK: Duration = Duration::from_millis(100);
/// Put before an utterance that continues the one answered last.
const CONTINUES_NOTE: &str =
    "(The speaker went on; their words below are the whole utterance, replacing the words you last answered:)";
/// Context lines (talk that wanted no reply) kept for the next input.
const MAX_CONTEXT_LINES: usize = 20;
const MAX_CONTEXT_CHARS: usize = 2_000;

type InfraSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// What the thread's session tells the conversation about its turns.
#[derive(Debug)]
pub(super) enum TurnSignal {
    Delta {
        turn_id: String,
        item_id: String,
        delta: String,
    },
    MessageDone {
        turn_id: String,
        item_id: String,
        text: String,
    },
    Finished {
        turn_id: Option<String>,
        /// It was aborted (interrupted, replaced) rather than completed.
        aborted: bool,
    },
    Tokens {
        input: i64,
        window: Option<i64>,
    },
}

/// The session's side of a running local-infra conversation.
#[derive(Clone)]
pub(super) struct LocalInfraHandle {
    signals: Sender<TurnSignal>,
}

impl RealtimeConversationManager {
    /// Hands the turn events of a local-infra conversation to it. True when
    /// one runs: the thread's turns are spoken by it, not mirrored to a
    /// realtime model.
    pub(crate) async fn local_infra_observe(&self, msg: &EventMsg) -> bool {
        let handle = {
            let state = self.state.lock().await;
            state
                .conversation
                .as_ref()
                .and_then(|conversation| conversation.local_infra.clone())
        };
        let Some(handle) = handle else {
            return false;
        };
        let signal = match msg {
            EventMsg::TurnComplete(event) => Some(TurnSignal::Finished {
                turn_id: Some(event.turn_id.clone()),
                aborted: false,
            }),
            EventMsg::TurnAborted(event) => Some(TurnSignal::Finished {
                turn_id: event.turn_id.clone(),
                aborted: true,
            }),
            EventMsg::AgentMessageContentDelta(event) => Some(TurnSignal::Delta {
                turn_id: event.turn_id.clone(),
                item_id: event.item_id.clone(),
                delta: event.delta.clone(),
            }),
            EventMsg::ItemCompleted(event) => match &event.item {
                TurnItem::AgentMessage(item) => Some(TurnSignal::MessageDone {
                    turn_id: event.turn_id.clone(),
                    item_id: item.id.clone(),
                    text: message_text(item),
                }),
                _ => None,
            },
            EventMsg::TokenCount(event) => event.info.as_ref().map(|info| TurnSignal::Tokens {
                input: info.last_token_usage.input_tokens,
                window: info.model_context_window,
            }),
            _ => None,
        };
        if let Some(signal) = signal {
            let _ = handle.signals.send(signal).await;
        }
        true
    }
}

fn message_text(item: &AgentMessageItem) -> String {
    item.content
        .iter()
        .map(|entry| match entry {
            codex_protocol::items::AgentMessageContent::Text { text } => text.as_str(),
        })
        .collect()
}

/// Starts a conversation of the thread over the configured local-infra
/// server. Failures are reported as a realtime `Error` event, as for the
/// other backends.
pub(super) async fn handle_start(
    sess: &Arc<Session>,
    sub_id: String,
    params: ConversationStartParams,
) -> CodexResult<()> {
    let config = sess.get_config().await;
    let infra = config.realtime.local_infra.clone();
    let socket = match connect(&infra).await {
        Ok(socket) => socket,
        Err(message) => {
            error!("failed to start local-infra realtime conversation: {message}");
            sess.send_event_raw(Event {
                id: sub_id,
                msg: EventMsg::RealtimeConversationRealtime(RealtimeConversationRealtimeEvent {
                    payload: RealtimeEvent::Error(message),
                }),
            })
            .await;
            return Ok(());
        }
    };

    let previous = sess.conversation.state.lock().await.conversation.take();
    if let Some(previous) = previous {
        stop_conversation_state(previous, RealtimeFanoutTaskStop::Await).await;
    }

    let (audio_tx, audio_rx) =
        async_channel::bounded::<RealtimeAudioFrame>(AUDIO_IN_QUEUE_CAPACITY);
    let (text_tx, text_rx) =
        async_channel::bounded::<ConversationTextParams>(TEXT_IN_QUEUE_CAPACITY);
    let (speech_tx, speech_rx) =
        async_channel::bounded::<RealtimeOutbound>(HANDOFF_OUT_QUEUE_CAPACITY);
    let (events_tx, events_rx) =
        async_channel::bounded::<RealtimeEvent>(OUTPUT_EVENTS_QUEUE_CAPACITY);
    let (signals_tx, signals_rx) = async_channel::unbounded::<TurnSignal>();
    let (out_tx, out_rx) = tokio::sync::mpsc::unbounded_channel::<Message>();
    let realtime_active = Arc::new(AtomicBool::new(true));
    let stop_token = CancellationToken::new();
    // Only `append_speech` uses it here: its speech arrives as outbound.
    let handoff = RealtimeHandoffState {
        output_tx: speech_tx,
        last_output: Arc::new(Mutex::new(None)),
        stream: Arc::new(Mutex::new(RealtimeHandoffStreamState::default())),
        client_managed_handoffs: true,
        host_routes_handoffs: false,
        codex_responses_as_items: false,
        codex_response_item_prefix: None,
        codex_response_handoff_mode: CodexResponseHandoffMode::default(),
        backend_reasoning_status: false,
        codex_response_handoff_channel_prefixes: Arc::new(BTreeMap::new()),
        session_kind: RealtimeSessionKind::V1,
        event_parser: RealtimeEventParser::V1,
    };
    let conversation = LocalInfraConversation {
        sess: Arc::downgrade(sess),
        sub_id: sub_id.clone(),
        group: infra
            .session
            .get("group")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        idle_compact_percent: infra
            .idle_compact_percent
            .filter(|percent| *percent <= 100)
            .unwrap_or(0),
        events_tx,
        out: out_tx,
        resampler: Resampler::default(),
        output_rate: OUTPUT_RATE,
        started: false,
        speaking: false,
        listening: false,
        turn: None,
        compacting: false,
        muted_turn: None,
        heard_since_cut: false,
        open_responses: HashMap::new(),
        items: HashMap::new(),
        context: Vec::new(),
        cut_off: None,
        last_answered: None,
        last_relays: Vec::new(),
        waiting: Vec::new(),
        waiting_since: None,
        relay_retry_at: None,
        idle_since: None,
        last_prompt: None,
        compact_floor: CompactFloor::None,
    };
    let input_task = tokio::spawn(conversation.run(
        socket,
        out_rx,
        infra,
        audio_rx,
        text_rx,
        speech_rx,
        signals_rx,
        stop_token.clone(),
    ));
    {
        let mut state = sess.conversation.state.lock().await;
        state.conversation = Some(ConversationState {
            audio_tx,
            text_tx,
            session_kind: RealtimeSessionKind::V1,
            handoff,
            input_task,
            fanout_task: None,
            realtime_active: Arc::clone(&realtime_active),
            route_handoffs: Arc::new(RealtimeHandoffAdmission::new()),
            stop_token,
            local_infra: Some(LocalInfraHandle {
                signals: signals_tx,
            }),
        });
        state.mode_instructions = Some(RealtimeModeInstructions {
            start: params.realtime_start_instructions,
            end: params.realtime_end_instructions,
        });
    }
    info!("local-infra realtime conversation connected");

    // Events go to the host until the conversation ends.
    let sess_clone = Arc::clone(sess);
    let fanout_active = Arc::clone(&realtime_active);
    let fanout_task = tokio::spawn(async move {
        let mut end = RealtimeConversationEnd::TransportClosed;
        while let Ok(event) = events_rx.recv().await {
            if let RealtimeEvent::Error(_) = &event {
                end = RealtimeConversationEnd::Error;
            }
            sess_clone
                .send_event_raw(Event {
                    id: sub_id.clone(),
                    msg: EventMsg::RealtimeConversationRealtime(
                        RealtimeConversationRealtimeEvent { payload: event },
                    ),
                })
                .await;
        }
        if fanout_active.swap(false, Ordering::Relaxed) {
            sess_clone
                .conversation
                .finish_if_active(&fanout_active)
                .await;
            send_realtime_conversation_closed(&sess_clone, sub_id, end).await;
        }
    });
    sess.conversation
        .register_fanout_task(&realtime_active, fanout_task)
        .await;
    Ok(())
}

/// Opens the server's WebSocket and asks for an audio mode session.
async fn connect(infra: &LocalInfraRealtimeConfig) -> Result<InfraSocket, String> {
    let url = infra
        .url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .ok_or("realtime.local_infra.url is not set")?;
    let mut request = url
        .into_client_request()
        .map_err(|err| format!("bad realtime.local_infra.url `{url}`: {err}"))?;
    if let Some(token) = infra.token.as_deref().filter(|token| !token.is_empty()) {
        let value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|err| format!("bad realtime.local_infra.token: {err}"))?;
        request.headers_mut().insert(AUTHORIZATION, value);
    }
    let mut config = infra.session.clone();
    if config
        .get("name")
        .and_then(serde_json::Value::as_str)
        .is_none_or(|name| name.trim().is_empty())
    {
        return Err("realtime.local_infra.session.name (the bot's name) is not set".to_string());
    }
    config.insert("mode".to_string(), json!("audio"));
    if let Some(path) = infra.ref_audio_path.as_deref().filter(|p| !p.is_empty()) {
        let bytes = tokio::fs::read(path).await.map_err(|err| {
            format!("cannot read realtime.local_infra.ref_audio_path `{path}`: {err}")
        })?;
        config.insert(
            "ref_audio".to_string(),
            json!(BASE64_STANDARD.encode(bytes)),
        );
        // The transcript belongs to that recording (the server's default
        // voice has its own).
        if let Some(text) = infra
            .ref_text
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
        {
            config.insert("ref_text".to_string(), json!(text));
        }
    }
    // Straight to the server (a local or LAN one), never through the
    // environment's proxy.
    let uri = request.uri();
    let host = uri
        .host()
        .unwrap_or_default()
        .trim_matches(['[', ']'])
        .to_string();
    let port = uri
        .port_u16()
        .unwrap_or(if uri.scheme_str() == Some("wss") {
            443
        } else {
            80
        });
    let connecting = async {
        let stream = tokio::net::TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|err| format!("cannot connect to {url}: {err}"))?;
        // 20 ms audio frames: no Nagle delay.
        let _ = stream.set_nodelay(true);
        tokio_tungstenite::client_async_tls(request, stream)
            .await
            .map_err(|err| format!("cannot connect to {url}: {err}"))
    };
    let (mut socket, _) = tokio::time::timeout(CONNECT_TIMEOUT, connecting)
        .await
        .map_err(|_| format!("connecting to {url} timed out"))??;
    let start = json!({"type": "session.start", "config": config});
    socket
        .send(Message::Text(start.to_string().into()))
        .await
        .map_err(|err| format!("cannot start the session at {url}: {err}"))?;
    Ok(socket)
}

struct LocalInfraConversation {
    sess: Weak<Session>,
    sub_id: String,
    /// A group conversation (a channel): talk that wants no reply is context.
    group: bool,
    idle_compact_percent: u8,
    events_tx: Sender<RealtimeEvent>,
    /// Messages to the server, sent in order by the socket's writer.
    out: tokio::sync::mpsc::UnboundedSender<Message>,
    resampler: Resampler,
    /// The server's speech sample rate (from `session.started`).
    output_rate: u32,
    /// The server's session has started (audio may flow).
    started: bool,
    speaking: bool,
    listening: bool,
    /// The turn (or compaction) of the thread this conversation runs, if
    /// any: only its text is spoken (a turn stopped for a newer one may
    /// still send some).
    turn: Option<String>,
    /// `turn` is a compaction (nothing is told into it).
    compacting: bool,
    /// A turn whose speech was cut (someone talked over it): nothing more of
    /// it is spoken, unless the talk that cut it turns out to be no
    /// utterance (a cough, noise).
    muted_turn: Option<String>,
    /// An utterance was heard since the last cut.
    heard_since_cut: bool,
    /// Responses given to the server and not done, with their turn.
    open_responses: HashMap<String, String>,
    /// The messages of the running turn being spoken, by item id.
    items: HashMap<String, SpokenItem>,
    /// Talk that wanted no reply (with its utterance id), for the next input.
    context: Vec<(u64, String)>,
    /// What the listener heard of replies of the current turn cut off.
    cut_off: Option<String>,
    /// The id of the last utterance answered.
    last_answered: Option<u64>,
    /// Appended words the running turn was started with: told again if it
    /// stops before it ends.
    last_relays: Vec<String>,
    /// Appended words waiting for a quiet moment, and since when.
    waiting: Vec<String>,
    waiting_since: Option<Instant>,
    /// A relay that could not be submitted is retried from then.
    relay_retry_at: Option<Instant>,
    /// Since when nothing happens (for compaction).
    idle_since: Option<Instant>,
    /// The last request's prompt size and the context window.
    last_prompt: Option<(i64, Option<i64>)>,
    /// How large the prompt must be before compacting (again).
    compact_floor: CompactFloor,
}

/// Keeps idle compaction from repeating: after one, the prompt must grow
/// again before the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CompactFloor {
    None,
    /// A compaction ran: the next prompt size sets the floor.
    AfterNextPrompt,
    Tokens(i64),
}

/// A message of the running turn, as far as it was spoken.
#[derive(Default)]
struct SpokenItem {
    /// Held back while it may still turn out to be `SILENCE_MARKER`.
    held: String,
    /// Some text was given to the server.
    sent: bool,
    /// It is (or starts with) the silence marker: nothing more is spoken.
    silent: bool,
}

enum Step {
    Continue,
    Stop,
}

impl LocalInfraConversation {
    #[allow(clippy::too_many_arguments)]
    async fn run(
        mut self,
        socket: InfraSocket,
        mut out_rx: tokio::sync::mpsc::UnboundedReceiver<Message>,
        infra: LocalInfraRealtimeConfig,
        audio_rx: Receiver<RealtimeAudioFrame>,
        text_rx: Receiver<ConversationTextParams>,
        speech_rx: Receiver<RealtimeOutbound>,
        signals_rx: Receiver<TurnSignal>,
        stop_token: CancellationToken,
    ) {
        let (mut sink, mut stream) = socket.split();
        let writer = tokio::spawn(async move {
            while let Some(message) = out_rx.recv().await {
                if let Err(err) = sink.send(message).await {
                    warn!("local-infra conversation: sending to the voice server failed: {err}");
                    break;
                }
            }
            let _ = sink.close().await;
        });
        let started_by = tokio::time::Instant::now() + START_TIMEOUT;
        let mut tick = tokio::time::interval(TICK);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        debug!(url = ?infra.url, "local-infra conversation running");
        loop {
            let step = tokio::select! {
                () = stop_token.cancelled() => {
                    self.send(json!({"type": "session.stop"}));
                    Step::Stop
                }
                message = stream.next() => match message {
                    Some(Ok(Message::Text(text))) => self.server_event(&text).await,
                    Some(Ok(Message::Binary(bytes))) => {
                        self.audio_out(&bytes).await;
                        Step::Continue
                    }
                    Some(Ok(Message::Close(_))) | None => {
                        self.emit(RealtimeEvent::Error("the voice server closed the connection".to_string())).await;
                        Step::Stop
                    }
                    Some(Ok(_)) => Step::Continue,
                    Some(Err(err)) => {
                        self.emit(RealtimeEvent::Error(format!("voice server connection failed: {err}"))).await;
                        Step::Stop
                    }
                },
                frame = audio_rx.recv() => match frame {
                    Ok(frame) => {
                        // The server takes audio once its session started.
                        if self.started
                            && let Some(pcm) = self.resampler.input_pcm(&frame)
                        {
                            let _ = self.out.send(Message::Binary(pcm.into()));
                        }
                        Step::Continue
                    }
                    Err(_) => Step::Stop,
                },
                text = text_rx.recv() => match text {
                    Ok(params) => {
                        self.wait_to_tell(params.text);
                        Step::Continue
                    }
                    Err(_) => Step::Stop,
                },
                speech = speech_rx.recv() => match speech {
                    Ok(RealtimeOutbound::StandaloneSpeech { text })
                    | Ok(RealtimeOutbound::HandoffUpdate { text, .. }) => {
                        self.wait_to_tell(text);
                        Step::Continue
                    }
                    Ok(_) => Step::Continue,
                    Err(_) => Step::Stop,
                },
                signal = signals_rx.recv() => match signal {
                    Ok(signal) => self.turn_signal(signal).await,
                    Err(_) => Step::Stop,
                },
                _ = tick.tick() => {
                    if !self.started && tokio::time::Instant::now() > started_by {
                        self.emit(RealtimeEvent::Error("the voice server did not start the session in time".to_string())).await;
                        Step::Stop
                    } else {
                        self.on_tick().await;
                        Step::Continue
                    }
                }
            };
            if let Step::Stop = step {
                break;
            }
        }
        // The writer ends once what was sent (a session.stop) is out.
        drop(self);
        let abort = writer.abort_handle();
        if tokio::time::timeout(Duration::from_secs(2), writer)
            .await
            .is_err()
        {
            abort.abort();
        }
        debug!("local-infra conversation ended");
    }

    async fn emit(&self, event: RealtimeEvent) {
        let _ = self.events_tx.send(event).await;
    }

    /// Sends `event` to the server (in order with the audio).
    fn send(&self, event: serde_json::Value) {
        let _ = self.out.send(Message::Text(event.to_string().into()));
    }

    /// An event of the server.
    async fn server_event(&mut self, text: &str) -> Step {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(text) else {
            warn!("local-infra conversation: unreadable server event");
            return Step::Continue;
        };
        let field = |key: &str| {
            event
                .get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
        };
        match field("type") {
            "session.started" => {
                self.started = true;
                if let Some(rate) = event
                    .get("output_rate")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|rate| u32::try_from(rate).ok())
                    .filter(|rate| (8_000..=48_000).contains(rate))
                {
                    self.output_rate = rate;
                }
                if let Some(sess) = self.sess.upgrade() {
                    sess.send_event_raw(Event {
                        id: self.sub_id.clone(),
                        msg: EventMsg::RealtimeConversationStarted(
                            RealtimeConversationStartedEvent {
                                realtime_session_id: None,
                                version: RealtimeConversationVersion::V1,
                            },
                        ),
                    })
                    .await;
                }
            }
            "input.speech_started" => {
                self.emit(RealtimeEvent::InputAudioSpeechStarted(
                    RealtimeInputAudioSpeechStarted { item_id: None },
                ))
                .await;
            }
            "input.transcript" => {
                let text = field("text").trim().to_string();
                if event.get("partial").and_then(serde_json::Value::as_bool) == Some(true) {
                    self.emit(RealtimeEvent::InputTranscriptDelta(
                        RealtimeTranscriptDelta { delta: text },
                    ))
                    .await;
                    return Step::Continue;
                }
                self.emit(RealtimeEvent::InputTranscriptDone(RealtimeTranscriptDone {
                    text: text.clone(),
                }))
                .await;
                if text.is_empty() {
                    return Step::Continue;
                }
                self.heard_since_cut = true;
                let respond = event
                    .get("respond")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(true);
                let id = event.get("id").and_then(serde_json::Value::as_u64);
                let replaces = event.get("replaces").and_then(serde_json::Value::as_u64);
                // What it continues was said already: it goes from the context.
                if let Some(replaced) = replaces {
                    self.context
                        .retain(|(context_id, _)| *context_id != replaced);
                }
                if respond {
                    let continues = replaces.is_some() && replaces == self.last_answered;
                    self.last_answered = id;
                    self.answer(&text, continues).await;
                } else if self.group {
                    self.keep_as_context(id.unwrap_or(u64::MAX), text);
                }
            }
            "state" => {
                let flag = |key: &str| {
                    event
                        .get(key)
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                };
                self.speaking = flag("speaking");
                let listening = flag("listening");
                // The talk that cut the bot ended without an utterance: the
                // turn is spoken again (its next messages).
                if self.listening && !listening && !self.heard_since_cut {
                    self.muted_turn = None;
                }
                self.listening = listening;
            }
            "response.text" => {
                self.emit(RealtimeEvent::OutputTranscriptDelta(
                    RealtimeTranscriptDelta {
                        delta: field("text").to_string(),
                    },
                ))
                .await;
            }
            "response.done" => {
                let response_id = field("response_id").to_string();
                let spoken = field("spoken").to_string();
                let cut = event.get("cut").and_then(serde_json::Value::as_bool) == Some(true);
                let turn = self.open_responses.remove(&response_id);
                // The server takes no more of it (a cut one included, should
                // its turn be spoken again).
                if let Some(item) = self.items.get_mut(&response_id) {
                    item.silent = true;
                }
                // What was heard of a cut reply goes with the next input, if
                // it was the current turn's (a turn already replaced says it
                // no longer).
                if cut && turn.is_some() && turn == self.turn {
                    let heard = self.cut_off.get_or_insert_with(String::new);
                    heard.push_str(&spoken);
                    // Talked over: nothing more of this turn is spoken (its
                    // later messages included) until an utterance answers,
                    // or the talk turns out to be none.
                    self.muted_turn = self.turn.clone();
                    self.heard_since_cut = false;
                }
                self.emit(RealtimeEvent::OutputTranscriptDone(
                    RealtimeTranscriptDone { text: spoken },
                ))
                .await;
                self.emit(if cut {
                    RealtimeEvent::ResponseCancelled(RealtimeResponseCancelled {
                        response_id: Some(response_id),
                    })
                } else {
                    RealtimeEvent::ResponseDone(RealtimeResponseDone {
                        response_id: Some(response_id),
                    })
                })
                .await;
            }
            "error" => {
                let message = field("message").to_string();
                warn!("local-infra voice server error: {message}");
                self.emit(RealtimeEvent::Error(message)).await;
                if !self.started {
                    return Step::Stop;
                }
            }
            _ => {}
        }
        Step::Continue
    }

    /// Speech of the server, for the host (24 kHz mono PCM).
    async fn audio_out(&self, bytes: &[u8]) {
        self.emit(RealtimeEvent::AudioOut(RealtimeAudioFrame {
            data: BASE64_STANDARD.encode(bytes),
            sample_rate: self.output_rate,
            num_channels: 1,
            samples_per_channel: Some((bytes.len() / 2) as u32),
            item_id: None,
        }))
        .await;
    }

    /// An utterance that wants a reply: whatever the bot was doing for an
    /// earlier one stops, and a turn answers it. `continues`: it replaces
    /// the utterance answered last (the speaker only paused).
    async fn answer(&mut self, text: &str, continues: bool) {
        let Some(sess) = self.sess.upgrade() else {
            return;
        };
        if let Some(turn) = self.turn.take() {
            // Only this conversation's own turn (another may have started).
            sess.abort_turn_if_active(&turn, TurnAbortReason::Interrupted)
                .await;
            self.compacting = false;
        }
        if !self.open_responses.is_empty() {
            self.send(json!({"type": "response.cancel"}));
        }
        self.items.clear();
        // Words the stopped turn was to tell go with this one.
        if !self.last_relays.is_empty() {
            let mut relays = std::mem::take(&mut self.last_relays);
            relays.append(&mut self.waiting);
            self.waiting = relays;
            self.waiting_since.get_or_insert_with(Instant::now);
        }
        let relays = self.waiting.clone();
        let waiting_since = self.waiting_since;
        let input = if continues {
            self.input_for(&format!("{CONTINUES_NOTE}\n{text}"))
        } else {
            self.input_for(text)
        };
        match self.start_turn(&sess, input).await {
            Ok(()) => self.last_relays = relays,
            Err(err) => {
                warn!("local-infra conversation: the turn did not start: {err}");
                // Still to be told.
                self.waiting = relays;
                self.waiting_since = waiting_since;
                self.emit(RealtimeEvent::Error(err)).await;
            }
        }
    }

    /// The turn input for `text`: what it carries first (host words still
    /// waiting for a quiet moment, talk that wanted no reply, what was heard
    /// of a reply cut off).
    fn input_for(&mut self, text: &str) -> String {
        let mut input = String::new();
        if !self.waiting.is_empty() {
            // Told with this turn rather than after it.
            input.push_str(&self.waiting.join("\n\n"));
            input.push('\n');
            self.waiting.clear();
            self.waiting_since = None;
        }
        if !self.context.is_empty() {
            let lines: Vec<&str> = self.context.iter().map(|(_, line)| line.as_str()).collect();
            input.push_str(&format!(
                "(Said meanwhile by others, not to you: {})\n",
                lines.join(" / ")
            ));
            self.context.clear();
        }
        match self.cut_off.take() {
            Some(heard) if heard.trim().is_empty() => {
                input.push_str(
                    "(Your last reply was cut off before the listener heard any of it.)\n",
                );
            }
            Some(heard) => {
                input.push_str(&format!(
                    "(Your last reply was cut off; the listener heard only: \"{heard}\")\n"
                ));
            }
            None => {}
        }
        input.push_str(text);
        input
    }

    async fn start_turn(&mut self, sess: &Arc<Session>, input: String) -> Result<(), String> {
        self.idle_since = None;
        let turn_id = sess.route_local_voice_input(input).await?;
        self.turn = Some(turn_id);
        Ok(())
    }

    fn keep_as_context(&mut self, id: u64, text: String) {
        self.context.push((id, text));
        while self.context.len() > MAX_CONTEXT_LINES
            || self
                .context
                .iter()
                .map(|(_, line)| line.chars().count())
                .sum::<usize>()
                > MAX_CONTEXT_CHARS
        {
            self.context.remove(0);
        }
    }

    /// Words of the host, told at the next quiet moment.
    fn wait_to_tell(&mut self, text: String) {
        if text.trim().is_empty() {
            return;
        }
        self.waiting.push(text);
        self.waiting_since.get_or_insert_with(Instant::now);
    }

    fn idle(&self) -> bool {
        self.started
            && self.turn.is_none()
            && !self.speaking
            && !self.listening
            && self.open_responses.is_empty()
    }

    async fn on_tick(&mut self) {
        let Some(sess) = self.sess.upgrade() else {
            return;
        };
        if !self.waiting.is_empty() {
            let late = self
                .waiting_since
                .is_some_and(|since| since.elapsed() > RELAY_WAIT);
            let retry_due = self.relay_retry_at.is_none_or(|at| Instant::now() >= at);
            // Told at a quiet moment, or (late) once no turn runs even if
            // someone is still talking; never steered into a running turn,
            // where an interruption would drop it. The next utterance takes
            // it along too (`input_for`).
            if retry_due && self.started && (self.idle() || (late && self.turn.is_none())) {
                let input = self.waiting.join("\n\n");
                match self.start_turn(&sess, input).await {
                    Ok(()) => {
                        self.last_relays = std::mem::take(&mut self.waiting);
                        self.waiting_since = None;
                        self.relay_retry_at = None;
                    }
                    Err(err) => {
                        warn!("local-infra conversation: not told yet: {err}");
                        self.relay_retry_at = Some(Instant::now() + RELAY_RETRY);
                    }
                }
            }
            return;
        }
        if !self.idle() {
            self.idle_since = None;
            return;
        }
        let idle_since = *self.idle_since.get_or_insert_with(Instant::now);
        if self.idle_compact_percent == 0 || idle_since.elapsed() < IDLE_BEFORE_COMPACT {
            return;
        }
        let Some((prompt, Some(window))) = self.last_prompt else {
            return;
        };
        let threshold = window * i64::from(self.idle_compact_percent) / 100;
        let floor = match self.compact_floor {
            CompactFloor::None => 0,
            CompactFloor::AfterNextPrompt => return,
            CompactFloor::Tokens(tokens) => tokens,
        };
        if window <= 0 || prompt < threshold.max(floor) {
            return;
        }
        // Not while the thread runs anything else.
        if sess.active_turn.lock().await.is_some() {
            return;
        }
        info!(
            prompt,
            window, "local-infra conversation: compacting the idle thread's history"
        );
        self.last_prompt = None;
        let turn_id = uuid::Uuid::now_v7().to_string();
        self.turn = Some(turn_id.clone());
        self.compacting = true;
        self.compact_floor = CompactFloor::None;
        let turn_context = sess
            .new_turn_with_default_settings(turn_id, Default::default())
            .await;
        sess.spawn_task(turn_context, Vec::new(), CompactTask).await;
    }

    async fn turn_signal(&mut self, signal: TurnSignal) -> Step {
        let current = |turn: &Option<String>, turn_id: &str| turn.as_deref() == Some(turn_id);
        match signal {
            TurnSignal::Finished { turn_id, aborted } => {
                // A turn stopped for a newer one ends on its own.
                if turn_id
                    .as_deref()
                    .is_some_and(|id| !current(&self.turn, id))
                {
                    return Step::Continue;
                }
                if self.compacting {
                    // Done: the next turn's prompt sets how far it must
                    // grow before another; stopped: tried again when idle.
                    self.compact_floor = if aborted {
                        CompactFloor::None
                    } else {
                        CompactFloor::AfterNextPrompt
                    };
                }
                if !aborted {
                    self.last_relays.clear();
                }
                self.turn = None;
                self.compacting = false;
                // A message left open by an aborted turn.
                let open: Vec<String> = self.items.drain().map(|(id, _)| id).collect();
                for item_id in open {
                    if self.open_responses.contains_key(&item_id) {
                        self.send(json!({"type": "response.end", "response_id": item_id}));
                    }
                }
            }
            TurnSignal::Delta { turn_id, .. } | TurnSignal::MessageDone { turn_id, .. }
                if !current(&self.turn, &turn_id)
                    || self.muted_turn.as_deref() == Some(turn_id.as_str()) => {}
            TurnSignal::Delta { item_id, delta, .. } => {
                let item = self.items.entry(item_id.clone()).or_default();
                if item.silent {
                    return Step::Continue;
                }
                let text = if item.sent {
                    delta
                } else {
                    item.held.push_str(&delta);
                    let held = item.held.trim_start();
                    if held.starts_with(SILENCE_MARKER) {
                        item.silent = true;
                        return Step::Continue;
                    }
                    if SILENCE_MARKER.starts_with(held) {
                        return Step::Continue; // may still be the marker
                    }
                    std::mem::take(&mut item.held)
                };
                item.sent = true;
                self.speak(&item_id, &text);
            }
            TurnSignal::MessageDone { item_id, text, .. } => {
                let item = self.items.remove(&item_id).unwrap_or_default();
                if item.silent {
                    return Step::Continue;
                }
                let rest = if item.sent {
                    String::new()
                } else if item.held.is_empty() {
                    // Not streamed: the whole message at once.
                    text
                } else {
                    item.held
                };
                let unsent = rest.trim_start();
                // Silence, or the start of the marker cut short.
                let silent = unsent.starts_with(SILENCE_MARKER)
                    || (!unsent.is_empty() && SILENCE_MARKER.starts_with(unsent.trim_end()));
                if !silent && !unsent.is_empty() {
                    self.speak(&item_id, &rest);
                }
                if self.open_responses.contains_key(&item_id) {
                    self.send(json!({"type": "response.end", "response_id": item_id}));
                }
            }
            // A compaction's own counts say nothing of the next prompt.
            TurnSignal::Tokens { .. } if self.compacting => {}
            TurnSignal::Tokens { input, window } => {
                self.last_prompt = Some((input, window));
                if self.compact_floor == CompactFloor::AfterNextPrompt {
                    // After a compaction the prompt must grow by a tenth of
                    // the window before the next one.
                    self.compact_floor =
                        CompactFloor::Tokens(input + window.unwrap_or(0).max(0) / 10);
                }
            }
        }
        Step::Continue
    }

    fn speak(&mut self, item_id: &str, text: &str) {
        let Some(turn) = self.turn.clone().filter(|_| !text.is_empty()) else {
            return;
        };
        self.open_responses.insert(item_id.to_string(), turn);
        self.send(json!({"type": "response.delta", "response_id": item_id, "text": text}));
    }
}

/// Host audio (any rate, any channel count, 16-bit PCM) to the server's
/// 16 kHz mono, keeping its position across frames.
#[derive(Default)]
struct Resampler {
    rate: u32,
    /// Where the next output sample lies, in input samples from the start of
    /// the next frame (may be negative: between the last two).
    position: f64,
    last: f32,
}

impl Resampler {
    fn input_pcm(&mut self, frame: &RealtimeAudioFrame) -> Option<Vec<u8>> {
        let bytes = BASE64_STANDARD.decode(&frame.data).ok()?;
        let channels = usize::from(frame.num_channels.max(1));
        let mono: Vec<f32> = bytes
            .chunks_exact(2 * channels)
            .map(|chunk| {
                chunk
                    .chunks_exact(2)
                    .map(|b| f32::from(i16::from_le_bytes([b[0], b[1]])))
                    .sum::<f32>()
                    / channels as f32
            })
            .collect();
        if mono.is_empty() || frame.sample_rate == 0 {
            return None;
        }
        if frame.sample_rate == INPUT_RATE {
            self.rate = INPUT_RATE;
            self.last = mono[mono.len() - 1];
            return Some(
                mono.iter()
                    .flat_map(|s| (s.round() as i16).to_le_bytes())
                    .collect(),
            );
        }
        if frame.sample_rate != self.rate {
            *self = Self {
                rate: frame.sample_rate,
                position: 0.0,
                last: mono[0],
            };
        }
        let step = f64::from(frame.sample_rate) / f64::from(INPUT_RATE);
        let mut out = Vec::with_capacity((mono.len() as f64 / step) as usize * 2 + 2);
        while self.position < mono.len() as f64 - 1.0 {
            let at = self.position;
            let sample = if at < 0.0 {
                let frac = (at + 1.0) as f32;
                self.last * (1.0 - frac) + mono[0] * frac
            } else {
                let index = at as usize;
                let frac = (at - index as f64) as f32;
                mono[index] * (1.0 - frac) + mono[index + 1] * frac
            };
            out.extend_from_slice(&(sample.round() as i16).to_le_bytes());
            self.position += step;
        }
        self.position -= mono.len() as f64;
        self.last = mono[mono.len() - 1];
        Some(out)
    }
}

#[cfg(test)]
#[path = "local_infra_tests.rs"]
mod tests;
