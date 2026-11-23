use super::*;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ReasoningItemContent;
use pretty_assertions::assert_eq;

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

#[test]
fn detects_only_unreadable_encrypted_content() {
    assert!(has_openai_encrypted_content(&reasoning(Some("gAAAA"))));
    assert!(!has_openai_encrypted_content(&reasoning(
        /*encrypted*/ None
    )));
    assert!(has_openai_encrypted_content(&ResponseItem::Compaction {
        id: None,
        encrypted_content: "gAAAA".to_string(),
        internal_chat_message_metadata_passthrough: None,
    }));
    assert!(has_openai_encrypted_content(&tool_output(vec![
        text_part("ok"),
        encrypted_part(),
    ])));
    assert!(!has_openai_encrypted_content(&tool_output(vec![
        text_part("ok")
    ])));
    // A message's own encrypted copy sits beside readable content.
    assert!(!has_openai_encrypted_content(&ResponseItem::Message {
        encrypted_content: Some("gAAAA".to_string()),
        status: None,
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "hi".to_string(),
            annotations: None,
            logprobs: None,
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }));
}

#[test]
fn strips_encrypted_items_and_replaces_encrypted_parts() {
    let mut input = vec![
        reasoning(Some("gAAAA")),
        reasoning(/*encrypted*/ None),
        ResponseItem::Compaction {
            id: None,
            encrypted_content: "gAAAA".to_string(),
            internal_chat_message_metadata_passthrough: None,
        },
        ResponseItem::ContextCompaction {
            id: None,
            encrypted_content: Some("gAAAA".to_string()),
            internal_chat_message_metadata_passthrough: None,
        },
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
        tool_output(vec![text_part("ok"), encrypted_part()]),
        tool_output(vec![encrypted_part()]),
    ];
    strip_openai_encrypted_content(&mut input);

    assert_eq!(
        input,
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
            tool_output(vec![text_part(ENCRYPTED_CONTENT_PLACEHOLDER)]),
        ]
    );
    assert!(!input.iter().any(has_openai_encrypted_content));
}
