//! Fork addition: additional context survives compaction.
//!
//! A value that does not change is sent once and then only kept by history;
//! compaction drops it, so without re-injection the model loses it for good
//! (for AstrBot: the persona in long conversations).

use anyhow::Result;
use codex_core::TurnInputRequest;
use codex_core::compact::SUMMARIZATION_PROMPT;
use codex_model_provider_info::built_in_model_providers;
use codex_protocol::protocol::AdditionalContextEntry;
use codex_protocol::protocol::AdditionalContextKind;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::TestCodex;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;

fn persona(value: &str) -> BTreeMap<String, AdditionalContextEntry> {
    BTreeMap::from([(
        "astrbot_persona".to_string(),
        AdditionalContextEntry {
            value: value.to_string(),
            kind: AdditionalContextKind::Application,
        },
    )])
}

async fn turn(test: &TestCodex, text: &str, value: &str) -> Result<()> {
    test.codex
        .start_or_steer_turn(
            TurnInputRequest::user_input(vec![UserInput::Text {
                text: text.to_string(),
                text_elements: Vec::new(),
            }])
            .with_additional_context(persona(value)),
        )
        .await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    Ok(())
}

/// Persona messages in the request after a turn, a manual compaction and
/// another turn with the same persona.
async fn personas_after_compaction(
    reinject: bool,
    max_value_tokens: Option<usize>,
    value: &str,
) -> Result<(Vec<String>, Vec<String>)> {
    let server = start_mock_server().await;
    let requests = mount_sse_sequence(
        &server,
        vec![
            sse(vec![ev_assistant_message("m1", "hi"), ev_completed("r1")]),
            sse(vec![
                ev_assistant_message("m2", "SUMMARY"),
                ev_completed("r2"),
            ]),
            sse(vec![ev_completed("r3")]),
        ],
    )
    .await;
    let mut provider = built_in_model_providers(/*openai_base_url*/ None)["openai"].clone();
    provider.name = "OpenAI (test)".into();
    provider.base_url = Some(format!("{}/v1", server.uri()));
    provider.supports_websockets = false;
    let test = test_codex()
        .with_config(move |config| {
            config.include_environment_context = false;
            config.model_provider = provider;
            config.compact_prompt = Some(SUMMARIZATION_PROMPT.to_string());
            config.additional_context.reinject_after_compaction = reinject;
            if let Some(max_value_tokens) = max_value_tokens {
                config.additional_context.max_value_tokens = max_value_tokens;
            }
        })
        .build(&server)
        .await?;

    turn(&test, "first turn", value).await?;
    test.codex.submit(Op::Compact).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    turn(&test, "after compaction", value).await?;

    let requests = requests.requests();
    assert_eq!(requests.len(), 3);
    let personas = |index: usize| -> Vec<String> {
        requests[index]
            .message_input_texts("developer")
            .into_iter()
            .filter(|text| text.starts_with("<astrbot_persona>"))
            .collect()
    };
    Ok((personas(0), personas(2)))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persona_survives_compaction_when_reinjected() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let (first, after) = personas_after_compaction(
        /*reinject*/ true, /*max_value_tokens*/ None, "be a cat",
    )
    .await?;
    let expected = vec!["<astrbot_persona>be a cat</astrbot_persona>".to_string()];
    assert_eq!(first, expected);
    // Back after compaction, once.
    assert_eq!(after, expected);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persona_is_lost_after_compaction_without_reinjection() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let (first, after) = personas_after_compaction(
        /*reinject*/ false, /*max_value_tokens*/ None, "be a cat",
    )
    .await?;
    assert_eq!(first.len(), 1);
    assert_eq!(after, Vec::<String>::new());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_raised_budget_keeps_a_long_persona_whole() -> Result<()> {
    skip_if_no_network!(Ok(()));

    // About 1500 tokens, past the default budget of 1000.
    let long = "persona line. ".repeat(430);
    let (first, after) = personas_after_compaction(/*reinject*/ true, Some(4_000), &long).await?;
    let expected = vec![format!("<astrbot_persona>{long}</astrbot_persona>")];
    assert_eq!(first, expected);
    assert_eq!(after, expected);
    Ok(())
}
