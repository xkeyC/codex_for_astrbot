use super::*;
use serde_json::json;

fn params(request: serde_json::Value) -> ConversationStartParams {
    serde_json::from_value::<RealtimeStartRequest>(request)
        .expect("valid request")
        .into_params()
}

fn defaults(transport: Option<ConversationStartTransport>) -> ConversationStartParams {
    ConversationStartParams {
        client_managed_handoffs: false,
        delegation_ack_filler: None,
        flush_transcript_tail_on_session_end: false,
        codex_responses_as_items: false,
        codex_response_item_prefix: None,
        codex_response_handoff_mode: CodexResponseHandoffMode::Thinking,
        codex_response_handoff_channel_prefixes: None,
        model: None,
        output_modality: RealtimeOutputModality::Audio,
        include_startup_context: true,
        initial_items: Vec::new(),
        realtime_start_instructions: None,
        realtime_end_instructions: None,
        prompt: None,
        realtime_session_id: None,
        transport,
        version: None,
        voice: None,
    }
}

#[test]
fn webrtc_request_uses_app_server_defaults() {
    assert_eq!(
        params(json!({"transport": {"type": "webrtc", "sdp": "v=0"}})),
        defaults(Some(ConversationStartTransport::Webrtc {
            sdp: "v=0".to_string()
        })),
    );
}

#[test]
fn empty_request_keeps_configured_transport() {
    assert_eq!(params(json!({})), defaults(None));
}

#[test]
fn existing_call_skips_startup_context_by_default() {
    let mut expected = defaults(Some(ConversationStartTransport::ExistingCall {
        call_id: "call_1".to_string(),
        sideband_base_url: None,
    }));
    expected.include_startup_context = false;
    assert_eq!(
        params(json!({"transport": {"type": "existing_call", "call_id": "call_1"}})),
        expected,
    );
}

#[test]
fn full_request_maps_every_field() {
    let request = json!({
        "transport": {"type": "websocket"},
        "output_modality": "text",
        "client_managed_handoffs": true,
        "include_startup_context": false,
        "initial_items": [
            {"text": "hi"},
            {"text": "be brief", "role": "developer"},
        ],
        "prompt": "You are on a voice channel.",
        "realtime_start_instructions": "start",
        "realtime_end_instructions": "end",
        "realtime_session_id": "rt_1",
        "model": "gpt-realtime-1.5",
        "version": "v1",
        "voice": "cove",
        "delegation_ack_filler": true,
        "flush_transcript_tail_on_session_end": true,
        "codex_responses_as_items": true,
        "codex_response_item_prefix": "codex:",
        "codex_response_handoff_mode": "bemTags",
        "codex_response_handoff_channel_prefixes": {"final": ["<f>"]},
    });
    let expected = ConversationStartParams {
        client_managed_handoffs: true,
        delegation_ack_filler: Some(true),
        flush_transcript_tail_on_session_end: true,
        codex_responses_as_items: true,
        codex_response_item_prefix: Some("codex:".to_string()),
        codex_response_handoff_mode: CodexResponseHandoffMode::BemTags,
        codex_response_handoff_channel_prefixes: Some(BTreeMap::from([(
            "final".to_string(),
            vec!["<f>".to_string()],
        )])),
        model: Some("gpt-realtime-1.5".to_string()),
        output_modality: RealtimeOutputModality::Text,
        include_startup_context: false,
        initial_items: vec![
            ConversationTextParams {
                text: "hi".to_string(),
                role: ConversationTextRole::User,
            },
            ConversationTextParams {
                text: "be brief".to_string(),
                role: ConversationTextRole::Developer,
            },
        ],
        realtime_start_instructions: Some("start".to_string()),
        realtime_end_instructions: Some("end".to_string()),
        prompt: Some(Some("You are on a voice channel.".to_string())),
        realtime_session_id: Some("rt_1".to_string()),
        transport: Some(ConversationStartTransport::Websocket),
        version: Some(RealtimeConversationVersion::V1),
        voice: Some(RealtimeVoice::Cove),
    };
    assert_eq!(params(request), expected);
}

#[test]
fn null_prompt_differs_from_absent_prompt() {
    let mut expected = defaults(None);
    expected.prompt = Some(None);
    assert_eq!(params(json!({"prompt": null})), expected);
}

#[test]
fn rejects_unknown_fields_and_transports() {
    for request in [
        json!({"transprot": {"type": "webrtc", "sdp": "v=0"}}),
        json!({"transport": {"type": "sip"}}),
        json!({"transport": {"type": "webrtc"}}),
        json!({"transport": {"type": "websocket", "sdp": "v=0"}}),
        json!({"initial_items": [{"text": "x", "rol": "developer"}]}),
    ] {
        assert!(
            serde_json::from_value::<RealtimeStartRequest>(request.clone()).is_err(),
            "accepted {request}",
        );
    }
}

#[test]
fn text_roles() {
    assert_eq!(
        ["user", "developer", "assistant"].map(|role| parse_text_role(role).expect("known role")),
        [
            ConversationTextRole::User,
            ConversationTextRole::Developer,
            ConversationTextRole::Assistant,
        ],
    );
    assert!(parse_text_role("system").is_err());
}
