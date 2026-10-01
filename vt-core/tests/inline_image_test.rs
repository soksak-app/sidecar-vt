use soksak_sidecar_vt_core::inline_image::{parse, Dimension, InlineImageCommand};

#[test]
fn file_payload_becomes_a_bounded_inline_image_with_typed_dimensions() {
    let command = parse(
        b"File=name=ZmlsZS5wbmc=;size=5;width=2px;height=50%;preserveAspectRatio=1;inline=1:aGVsbG8=",
    )
    .expect("valid inline image");
    assert_eq!(
        command,
        InlineImageCommand::Display {
            name: "file.png".into(),
            data: b"hello".to_vec(),
            width: Dimension::Pixels(2),
            height: Dimension::Percent(50),
            preserve_aspect_ratio: true,
        }
    );
}

#[test]
fn non_inline_file_is_an_explicit_transfer_outcome() {
    let command = parse(b"File=name=ZmlsZS50eHQ=;inline=0:dGV4dA==").unwrap();
    assert_eq!(
        command,
        InlineImageCommand::Transfer {
            name: Some("file.txt".into()),
            data: b"text".to_vec(),
        }
    );
}

#[test]
fn malformed_size_base64_and_dimensions_are_rejected() {
    for payload in [
        b"File=size=4;inline=1:aGVsbG8=".as_slice(),
        b"File=size=6;inline=1:aGVsbG8=".as_slice(),
        b"File=inline=1:%%%".as_slice(),
        b"File=inline=1;width=0:ZGF0YQ==".as_slice(),
        b"File=inline=1;width=2em:ZGF0YQ==".as_slice(),
    ] {
        assert!(
            parse(payload).is_err(),
            "accepted malformed payload {payload:?}"
        );
    }
}

#[test]
fn multipart_records_are_typed_and_do_not_become_a_display_by_fallback() {
    assert_eq!(
        parse(b"MultipartFile=name=ZmlsZS5wbmc=;inline=1").unwrap(),
        InlineImageCommand::MultipartStart {
            name: "file.png".into(),
        }
    );
    assert_eq!(
        parse(b"FilePart:aGVsbG8=").unwrap(),
        InlineImageCommand::MultipartPart(b"hello".to_vec())
    );
    assert_eq!(parse(b"FileEnd").unwrap(), InlineImageCommand::MultipartEnd);
}

#[test]
fn oversized_encoded_payload_is_rejected_before_decoding() {
    let mut payload = b"File=inline=1:".to_vec();
    payload.extend(std::iter::repeat_n(b'A', 1_398_105));
    let error = parse(&payload).expect_err("oversized payload must be rejected");
    assert!(error.contains("exceeds"), "unexpected error: {error}");
}
