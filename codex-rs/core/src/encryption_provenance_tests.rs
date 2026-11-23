use super::*;
use codex_protocol::models::ReasoningItemContent;
use pretty_assertions::assert_eq;

fn identity(provider: &str, account: Option<&str>) -> EncryptionOwner {
    EncryptionOwner {
        provider: provider.to_string(),
        account: account.map(str::to_string),
    }
}

fn provenance(current: EncryptionOwner, legacy_provider: Option<&str>) -> EncryptionProvenance {
    EncryptionProvenance {
        current: Some(current),
        legacy_provider: legacy_provider.map(str::to_string),
    }
}

fn reasoning(encrypted: Option<&str>) -> ResponseItem {
    ResponseItem::Reasoning {
        id: None,
        summary: Vec::new(),
        content: Some(vec![ReasoningItemContent::ReasoningText {
            text: "thinking".to_string(),
        }]),
        encrypted_content: encrypted.map(str::to_string),
        internal_chat_message_metadata_passthrough: None,
    }
}

fn tool_output(parts: Vec<FunctionCallOutputContentItem>) -> ResponseItem {
    ResponseItem::FunctionCallOutput {
        id: None,
        call_id: Some("call-1".to_string()),
        name: None,
        namespace: None,
        output: FunctionCallOutputPayload::from_content_items(parts),
        internal_chat_message_metadata_passthrough: None,
    }
}

fn encrypted_part() -> FunctionCallOutputContentItem {
    FunctionCallOutputContentItem::EncryptedContent {
        encrypted_content: "gAAAA".to_string(),
    }
}

fn text_part(text: &str) -> FunctionCallOutputContentItem {
    FunctionCallOutputContentItem::InputText {
        text: text.to_string(),
    }
}

fn owned(item: ResponseItem, owner: &EncryptionOwner) -> ResponseItemEnvelope {
    let mut envelope = ResponseItemEnvelope::new(item);
    set_owner(&mut envelope.metadata, owner.clone());
    envelope
}

fn items(envelopes: &[ResponseItemEnvelope]) -> Vec<ResponseItem> {
    envelopes
        .iter()
        .map(|envelope| envelope.item.clone())
        .collect()
}

#[test]
fn owners_tell_what_is_foreign() {
    let openai = identity("openai", Some("chatgpt:acct-1"));
    let deepseek = identity("deepseek", Some("key:0123456789abcdef"));
    let state = provenance(deepseek.clone(), Some("openai"));
    // DeepSeek's own reasoning carries a token of its own.
    assert!(!state.has_foreign_encrypted_content(&owned(reasoning(Some("3a94-0")), &deepseek)));
    assert!(state.has_foreign_encrypted_content(&owned(reasoning(Some("gAAAA")), &openai)));
    // Another key of the same provider is another identity.
    let other_key = identity("deepseek", Some("key:fedcba9876543210"));
    assert!(state.is_foreign(&owned(reasoning(Some("3a94-0")), &other_key)));
    // Unmarked history is the SessionMeta provider's, account unknown.
    let legacy = ResponseItemEnvelope::new(reasoning(Some("gAAAA")));
    assert!(state.is_foreign(&legacy));
    assert!(
        !provenance(identity("openai", Some("chatgpt:acct-2")), Some("openai")).is_foreign(&legacy)
    );
    assert!(!provenance(deepseek, /*legacy_provider*/ None).is_foreign(&legacy));
    // Nothing is foreign before the identity is known.
    assert!(!EncryptionProvenance::default().is_foreign(&owned(reasoning(Some("gAAAA")), &openai)));
}

#[test]
fn marks_only_items_with_encrypted_content() {
    let deepseek = identity("deepseek", None);
    let state = provenance(deepseek.clone(), /*legacy_provider*/ None);
    let openai = identity("openai", None);
    let mut envelopes = [
        ResponseItemEnvelope::new(reasoning(Some("3a94-0"))),
        ResponseItemEnvelope::new(reasoning(/*encrypted*/ None)),
        owned(reasoning(Some("gAAAA")), &openai),
    ];
    state.mark_produced(envelopes.iter_mut());
    assert_eq!(
        envelopes.iter().map(owner).collect::<Vec<_>>(),
        vec![Some(&deepseek), None, Some(&openai)]
    );
}

#[test]
fn strips_foreign_items_and_keeps_the_current_identitys() {
    let openai = identity("openai", Some("chatgpt:acct-1"));
    let deepseek = identity("deepseek", Some("key:0123456789abcdef"));
    let state = provenance(deepseek.clone(), /*legacy_provider*/ None);
    let own_reasoning = reasoning(Some("3a94aa8f-0"));
    let mut input = vec![
        owned(reasoning(Some("gAAAA")), &openai),
        ResponseItemEnvelope::new(reasoning(/*encrypted*/ None)),
        owned(
            ResponseItem::Compaction {
                id: None,
                encrypted_content: "gAAAA".to_string(),
                internal_chat_message_metadata_passthrough: None,
            },
            &openai,
        ),
        owned(
            ResponseItem::AgentMessage {
                id: None,
                author: "/root/worker".to_string(),
                recipient: "/root".to_string(),
                content: vec![
                    AgentMessageInputContent::InputText {
                        text: "done".to_string(),
                    },
                    AgentMessageInputContent::EncryptedContent {
                        encrypted_content: "gAAAA".to_string(),
                    },
                ],
                internal_chat_message_metadata_passthrough: None,
            },
            &openai,
        ),
        owned(
            tool_output(vec![text_part("ok"), encrypted_part()]),
            &openai,
        ),
        owned(own_reasoning.clone(), &deepseek),
        owned(tool_output(vec![encrypted_part()]), &deepseek),
    ];
    state.strip_foreign(&mut input);

    assert_eq!(
        items(&input),
        vec![
            reasoning(/*encrypted*/ None),
            ResponseItem::AgentMessage {
                id: None,
                author: "/root/worker".to_string(),
                recipient: "/root".to_string(),
                content: vec![
                    AgentMessageInputContent::InputText {
                        text: "done".to_string(),
                    },
                    AgentMessageInputContent::InputText {
                        text: ENCRYPTED_CONTENT_PLACEHOLDER.to_string(),
                    },
                ],
                internal_chat_message_metadata_passthrough: None,
            },
            tool_output(vec![
                text_part("ok"),
                text_part(ENCRYPTED_CONTENT_PLACEHOLDER)
            ]),
            own_reasoning,
            tool_output(vec![encrypted_part()]),
        ]
    );
    assert!(
        !input
            .iter()
            .any(|item| state.has_foreign_encrypted_content(item))
    );
}

#[test]
fn key_hashes_are_short_and_stable() {
    let mut headers = http::HeaderMap::new();
    headers.insert("authorization", "Bearer sk-secret".parse().expect("header"));
    let hash = key_hash(&headers).expect("a hash");
    assert_eq!(hash.len(), "key:".len() + KEY_HASH_LEN);
    assert!(!hash.contains("secret"));
    assert_eq!(key_hash(&headers), Some(hash.clone()));
    headers.insert("authorization", "Bearer sk-other".parse().expect("header"));
    assert_ne!(key_hash(&headers), Some(hash));
    assert_eq!(key_hash(&http::HeaderMap::new()), None);
}
