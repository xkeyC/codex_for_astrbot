use std::fmt::Display;

use codex_utils_absolute_path::AbsolutePathBuf;

const GENERATED_IMAGE_ARTIFACTS_DIR: &str = "generated_images";
const MAX_IMAGE_GENERATION_OUTPUT_HINT_BYTES: usize = 1024;

/// Returns the extension-owned artifact path for a generated image.
pub(crate) fn image_generation_artifact_path(
    save_root: &AbsolutePathBuf,
    session_id: &str,
    call_id: &str,
) -> AbsolutePathBuf {
    let sanitize = |value: &str| {
        let mut sanitized: String = value
            .chars()
            .map(|ch| {
                if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                    ch
                } else {
                    '_'
                }
            })
            .collect();
        if sanitized.is_empty() {
            sanitized = "generated_image".to_string();
        }
        sanitized
    };

    save_root
        .join(GENERATED_IMAGE_ARTIFACTS_DIR)
        .join(sanitize(session_id))
        .join(format!("{}.png", sanitize(call_id)))
}

/// Returns the model-facing generated-image path hint, or omits it if it is too large.
///
/// Fork change: upstream tells the model the image "is already displayed to the
/// user", which holds in the TUI. Under AstrBot nothing shows it, and the save
/// directory lives in CODEX_HOME on the host, outside every tool the model has.
/// AstrBot copies the image into the chat's workspace and then tells the model
/// where; this hint only has to keep it from claiming the image was delivered
/// in the meantime.
pub(crate) fn image_generation_output_hint(
    image_output_dir: impl Display,
    image_output_path: impl Display,
) -> Option<String> {
    let hint = format!(
        "The image was generated and saved on the host as {image_output_path} (in {image_output_dir}), which none of your tools can reach.\nIt has NOT been shown to the user. A copy is being placed in your workspace and you will be told its path; send it from there, or keep working with it there.\nDo not tell the user the image is already visible, and do not render it as a Markdown image or file link."
    );
    (hint.len() <= MAX_IMAGE_GENERATION_OUTPUT_HINT_BYTES).then_some(hint)
}
