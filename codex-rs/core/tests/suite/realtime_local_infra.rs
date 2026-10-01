//! AstrBot: realtime voice over local-multimodal-infra
//! (`[realtime] backend = "local_multimodal_infra"`), against a scripted
//! voice server and a mocked model.

use anyhow::Result;
use codex_config::realtime_local_infra::RealtimeBackend;
use codex_protocol::protocol::ConversationStartParams;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::RealtimeOutputModality;
use core_test_support::responses;
use core_test_support::responses::WebSocketTestServer;
use core_test_support::responses::start_mock_server;
use core_test_support::responses::start_websocket_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use core_test_support::wait_for_event_match;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::time::Duration;

fn start_params() -> ConversationStartParams {
    ConversationStartParams {
        backend_reasoning_status: false,
        client_managed_handoffs: false,
        delegation_ack_filler: None,
        flush_transcript_tail_on_session_end: false,
        codex_responses_as_items: false,
        codex_response_item_prefix: None,
        codex_response_handoff_mode: codex_protocol::protocol::CodexResponseHandoffMode::Thinking,
        codex_response_handoff_channel_prefixes: None,
        model: None,
        output_modality: RealtimeOutputModality::Audio,
        include_startup_context: false,
        initial_items: Vec::new(),
        realtime_start_instructions: Some("A voice call started.".to_string()),
        realtime_end_instructions: Some("The voice call ended.".to_string()),
        prompt: None,
        realtime_session_id: None,
        transport: None,
        version: None,
        voice: None,
    }
}

/// The JSON messages the voice server has received, in order.
fn received(server: &WebSocketTestServer) -> Vec<Value> {
    server
        .connections()
        .iter()
        .flat_map(|connection| connection.iter())
        .map(core_test_support::responses::WebSocketRequest::body_json)
        .collect()
}

async fn wait_for_received(server: &WebSocketTestServer, count: usize) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let messages = received(server);
        if messages.len() >= count {
            return messages;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the voice server got only {messages:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_the_server_hears_is_answered_by_a_turn_and_spoken() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let api_server = start_mock_server().await;
    let response_mock = responses::mount_sse_once(
        &api_server,
        responses::sse(vec![
            responses::ev_response_created("resp-1"),
            responses::ev_assistant_message("msg-1", "It is three."),
            responses::ev_completed("resp-1"),
        ]),
    )
    .await;
    let voice_server = start_websocket_server(vec![vec![
        // session.start
        vec![
            json!({"type": "session.started", "input_rate": 16000, "output_rate": 24000}),
            json!({"type": "input.transcript", "id": 0, "text": "what time is it", "respond": true}),
        ],
        // response.delta
        vec![],
        // response.end
        vec![json!({"type": "response.done", "response_id": "msg-1", "spoken": "It is three.", "cut": false})],
        // Kept open.
        vec![],
    ]])
    .await;

    let voice_dir = tempfile::tempdir()?;
    let ref_audio = voice_dir.path().join("voice.wav");
    std::fs::write(&ref_audio, b"RIFF")?;
    let mut builder = test_codex().with_config({
        let url = voice_server.uri().to_string();
        move |config| {
            config.realtime.backend = RealtimeBackend::LocalMultimodalInfra;
            config.realtime.local_infra.url = Some(url);
            config.realtime.local_infra.token = Some("infer-token".to_string());
            config.realtime.local_infra.ref_audio_path =
                Some(ref_audio.to_string_lossy().into_owned());
            config.realtime.local_infra.ref_text = Some(" Hello there. ".to_string());
            config
                .realtime
                .local_infra
                .session
                .insert("name".to_string(), json!("Xiaole"));
        }
    });
    let test = builder.build(&api_server).await?;
    test.codex
        .submit(Op::RealtimeConversationStart(start_params()))
        .await?;
    wait_for_event(&test.codex, |msg| {
        matches!(msg, EventMsg::RealtimeConversationStarted(_))
    })
    .await;

    let messages = wait_for_received(&voice_server, 3).await;
    assert_eq!(messages[0]["type"], "session.start");
    assert_eq!(messages[0]["config"]["mode"], "audio");
    assert_eq!(messages[0]["config"]["name"], "Xiaole");
    assert_eq!(messages[0]["config"]["ref_audio"], "UklGRg==");
    assert_eq!(messages[0]["config"]["ref_text"], "Hello there.");
    let item = messages[1]["response_id"].clone();
    assert_eq!(
        messages[1..3],
        [
            json!({"type": "response.delta", "response_id": item, "text": "It is three."}),
            json!({"type": "response.end", "response_id": item}),
        ]
    );
    assert_eq!(
        voice_server.single_handshake().header("authorization"),
        Some("Bearer infer-token".to_string())
    );

    // The turn read what was heard, as a realtime turn.
    let request = response_mock.single_request();
    let user_texts = request.message_input_texts("user");
    assert!(
        user_texts.iter().any(|text| text == "what time is it"),
        "{user_texts:?}"
    );
    let metadata: Value = serde_json::from_str(
        request
            .header("x-codex-turn-metadata")
            .as_deref()
            .expect("turn metadata"),
    )?;
    assert_eq!(metadata["turn_trigger"], "realtime");

    // The host hears what was said.
    let spoken = wait_for_event_match(&test.codex, |msg| match msg {
        EventMsg::RealtimeConversationRealtime(event) => match &event.payload {
            codex_protocol::protocol::RealtimeEvent::OutputTranscriptDone(done) => {
                Some(done.text.clone())
            }
            _ => None,
        },
        _ => None,
    })
    .await;
    assert_eq!(spoken, "It is three.");

    voice_server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_silence_marker_is_not_spoken() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let api_server = start_mock_server().await;
    let response_mock = responses::mount_sse_once(
        &api_server,
        responses::sse(vec![
            responses::ev_response_created("resp-1"),
            responses::ev_assistant_message("msg-1", "<silence>"),
            responses::ev_completed("resp-1"),
        ]),
    )
    .await;
    let voice_server = start_websocket_server(vec![vec![
        vec![
            json!({"type": "session.started", "input_rate": 16000, "output_rate": 24000}),
            json!({"type": "input.transcript", "id": 0, "text": "mm", "respond": true}),
        ],
        // Open for anything spoken.
        vec![],
    ]])
    .await;

    let mut builder = test_codex().with_config({
        let url = voice_server.uri().to_string();
        move |config| {
            config.realtime.backend = RealtimeBackend::LocalMultimodalInfra;
            config.realtime.local_infra.url = Some(url);
            config
                .realtime
                .local_infra
                .session
                .insert("name".to_string(), json!("Xiaole"));
        }
    });
    let test = builder.build(&api_server).await?;
    test.codex
        .submit(Op::RealtimeConversationStart(start_params()))
        .await?;
    wait_for_event(&test.codex, |msg| matches!(msg, EventMsg::TurnComplete(_))).await;
    assert_eq!(response_mock.requests().len(), 1);
    tokio::time::sleep(Duration::from_millis(300)).await;
    let messages = received(&voice_server);
    assert_eq!(messages.len(), 1, "{messages:?}");
    assert_eq!(messages[0]["type"], "session.start");

    voice_server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hang_up_is_told_with_its_time_and_the_next_call_starts_anew() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let api_server = start_mock_server().await;
    let answer = |id: &str| {
        responses::sse(vec![
            responses::ev_response_created(id),
            responses::ev_assistant_message(&format!("msg-{id}"), "Hi."),
            responses::ev_completed(id),
        ])
    };
    let response_mock =
        responses::mount_sse_sequence(&api_server, vec![answer("resp-1"), answer("resp-2")]).await;
    let call = |text: &str| {
        vec![
            // session.start
            vec![
                json!({"type": "session.started", "input_rate": 16000, "output_rate": 24000}),
                json!({"type": "input.transcript", "id": 0, "text": text, "respond": true}),
            ],
            // response.delta, response.end, session.stop
            vec![],
            vec![],
            vec![],
        ]
    };
    let voice_server = start_websocket_server(vec![call("hello"), call("hello again")]).await;

    let mut builder = test_codex().with_config({
        let url = voice_server.uri().to_string();
        move |config| {
            config.realtime.backend = RealtimeBackend::LocalMultimodalInfra;
            config.realtime.local_infra.url = Some(url);
            config
                .realtime
                .local_infra
                .session
                .insert("name".to_string(), json!("Xiaole"));
        }
    });
    let test = builder.build(&api_server).await?;
    let params = || ConversationStartParams {
        realtime_end_instructions: Some("The voice call ended at {now}.".to_string()),
        ..start_params()
    };
    for _ in 0..2 {
        test.codex
            .submit(Op::RealtimeConversationStart(params()))
            .await?;
        wait_for_event(&test.codex, |msg| matches!(msg, EventMsg::TurnComplete(_))).await;
        test.codex.submit(Op::RealtimeConversationClose).await?;
        wait_for_event(&test.codex, |msg| {
            matches!(msg, EventMsg::RealtimeConversationClosed(_))
        })
        .await;
    }

    // The second call's turn reads the first one's end, with its time, and
    // a start of its own.
    let requests = response_mock.requests();
    assert_eq!(requests.len(), 2);
    let developer = requests[1].message_input_texts("developer").join(
        "
",
    );
    assert_eq!(
        developer.matches("A voice call started.").count(),
        2,
        "{developer}"
    );
    let ended = developer
        .split("The voice call ended at ")
        .nth(1)
        .expect("the end of the first call");
    assert!(
        ended.starts_with("20") && !ended.starts_with("{now}"),
        "{developer}"
    );
    assert!(
        developer.find("The voice call ended at").unwrap()
            < developer.rfind("A voice call started.").unwrap(),
        "{developer}"
    );

    voice_server.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_server_url_is_reported() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let api_server = start_mock_server().await;
    let mut builder = test_codex().with_config(|config| {
        config.realtime.backend = RealtimeBackend::LocalMultimodalInfra;
    });
    let test = builder.build(&api_server).await?;
    test.codex
        .submit(Op::RealtimeConversationStart(start_params()))
        .await?;
    let error = wait_for_event_match(&test.codex, |msg| match msg {
        EventMsg::RealtimeConversationRealtime(event) => match &event.payload {
            codex_protocol::protocol::RealtimeEvent::Error(message) => Some(message.clone()),
            _ => None,
        },
        _ => None,
    })
    .await;
    assert!(error.contains("realtime.local_infra.url"), "{error}");
    Ok(())
}
