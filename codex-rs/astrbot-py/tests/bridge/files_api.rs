//! AstrBot: a provider's images through its Files API
//! (`[model_provider_options.<id>] files_api = {}`), against a mocked
//! `/files` and `/responses`.

use anyhow::Result;
use codex_config::model_provider_options::FilesApiOptions;
use codex_config::model_provider_options::ModelProviderOptions;
use codex_protocol::models::ImageReference;
use codex_protocol::protocol::EventMsg;
use codex_protocol::turn_input::TurnInputRequest;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path_regex;

const IMAGE: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_image_is_uploaded_once_and_referenced_by_file_id() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    Mock::given(method("POST"))
        .and(path_regex(".*/files$"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "file-api-test", "object": "file", "bytes": 70,
            "filename": "image.png", "purpose": "user_data"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-1"),
                ev_assistant_message("msg-1", "A dot."),
                ev_completed("resp-1"),
            ]),
            sse(vec![
                ev_response_created("resp-2"),
                ev_assistant_message("msg-2", "Still a dot."),
                ev_completed("resp-2"),
            ]),
        ],
    )
    .await;

    let mut builder = test_codex().with_config(|config| {
        config.model_provider_options.insert(
            config.model_provider_id.clone(),
            ModelProviderOptions {
                files_api: Some(FilesApiOptions::default()),
                ..Default::default()
            },
        );
    });
    let test = builder.build(&server).await?;

    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![
            UserInput::Image {
                image: ImageReference::Inline {
                    image_url: IMAGE.to_string(),
                },
                detail: None,
            },
            UserInput::Text {
                text: "what is this?".into(),
                text_elements: Vec::new(),
            },
        ]))
        .await?;
    wait_for_event(&test.codex, |msg| matches!(msg, EventMsg::TurnComplete(_))).await;
    test.submit_text_turn("and now?").await?;

    // Both requests carry the image by its file id, never inline.
    let requests = responses.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        let body = request.body_json().to_string();
        assert!(body.contains(r#""file_id":"file-api-test""#), "{body}");
        assert!(!body.contains("base64"), "{body}");
    }

    let uploads: Vec<String> = server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|request| request.url.path().ends_with("/files"))
        .map(|request| String::from_utf8_lossy(&request.body).into_owned())
        .collect();
    assert_eq!(uploads.len(), 1);
    assert!(uploads[0].contains("name=\"purpose\"\r\n\r\nuser_data"));
    assert!(uploads[0].contains("name=\"expires_after[seconds]\"\r\n\r\n86400"));
    assert!(uploads[0].contains("Content-Type: image/png"));
    Ok(())
}
