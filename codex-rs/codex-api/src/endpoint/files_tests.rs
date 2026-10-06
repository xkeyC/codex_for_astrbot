use super::*;
use pretty_assertions::assert_eq;

#[test]
fn decodes_base64_image_data_urls_only() {
    assert_eq!(
        decode_image_data_url("data:image/png;base64,AAEC"),
        Some(("image/png".to_string(), vec![0, 1, 2]))
    );
    assert_eq!(decode_image_data_url("data:text/plain;base64,AAEC"), None);
    assert_eq!(decode_image_data_url("data:image/png,raw"), None);
    assert_eq!(decode_image_data_url("https://example.com/a.png"), None);
    assert_eq!(decode_image_data_url("data:image/png;base64,***"), None);
}

#[test]
fn upload_body_is_a_multipart_form() {
    let body = String::from_utf8(upload_body("image/jpeg", b"JPEG", Some(3600))).unwrap();
    let b = MULTIPART_BOUNDARY;
    assert_eq!(
        body,
        format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"purpose\"\r\n\r\nuser_data\r\n\
             --{b}\r\nContent-Disposition: form-data; name=\"expires_after[anchor]\"\r\n\r\ncreated_at\r\n\
             --{b}\r\nContent-Disposition: form-data; name=\"expires_after[seconds]\"\r\n\r\n3600\r\n\
             --{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"image.jpg\"\r\n\
             Content-Type: image/jpeg\r\n\r\nJPEG\r\n--{b}--\r\n"
        )
    );
}

#[test]
fn upload_body_without_expiry_keeps_the_file() {
    let body = String::from_utf8(upload_body("image/png", b"PNG", None)).unwrap();
    assert!(!body.contains("expires_after"));
    assert!(body.contains("filename=\"image.png\""));
}
