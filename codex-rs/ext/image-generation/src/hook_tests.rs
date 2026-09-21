//! Fork addition: the saved-image hook lets the host answer in the tool result.

use std::sync::Arc;
use std::sync::Mutex;

use codex_model_provider::create_model_provider;
use codex_model_provider_info::ModelProviderInfo;
use codex_utils_absolute_path::AbsolutePathBuf;
use pretty_assertions::assert_eq;

use crate::SavedImage;
use crate::SavedImageHook;
use crate::backend::CodexImagesBackend;
use crate::tool::ImageGenerationTool;

fn tool(hook: Option<SavedImageHook>) -> ImageGenerationTool {
    let provider = create_model_provider(
        ModelProviderInfo::create_openai_provider(/*base_url*/ None),
        /*auth_manager*/ None,
    );
    ImageGenerationTool::new(
        CodexImagesBackend::new(provider, /*originator*/ None),
        /*save_root*/ None,
        "thread-1".to_string(),
        hook,
    )
}

fn saved_path() -> AbsolutePathBuf {
    let path = std::env::temp_dir().join("generated_images/thread-1/call-1.png");
    AbsolutePathBuf::try_from(path).expect("temp dir is absolute")
}

#[tokio::test]
async fn the_host_hint_replaces_the_built_in_one() {
    let seen: Arc<Mutex<Vec<SavedImage>>> = Arc::default();
    let recorded = Arc::clone(&seen);
    let hook: SavedImageHook = Arc::new(move |image| {
        recorded.lock().expect("lock").push(image);
        Box::pin(async { Some("copied to generated_images/call-1.png".to_string()) })
    });

    let hint = tool(Some(hook)).host_hint("call-1", &saved_path()).await;

    assert_eq!(
        hint.as_deref(),
        Some("copied to generated_images/call-1.png")
    );
    let seen = seen.lock().expect("lock");
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].thread_id, "thread-1");
    assert_eq!(seen[0].call_id, "call-1");
    assert_eq!(seen[0].saved_path, saved_path());
}

#[tokio::test]
async fn a_hook_that_declines_keeps_the_built_in_hint() {
    let hook: SavedImageHook = Arc::new(|_image| Box::pin(async { None }));

    assert_eq!(
        tool(Some(hook)).host_hint("call-1", &saved_path()).await,
        None
    );
}

#[tokio::test]
async fn no_hook_means_no_host_hint() {
    assert_eq!(tool(None).host_hint("call-1", &saved_path()).await, None);
}
