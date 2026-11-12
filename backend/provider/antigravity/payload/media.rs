//! Validation and extraction of inline media for Antigravity requests.

use crate::openai::{ChatMessage, ContentPart, MessageContent};
use base64::engine::general_purpose::STANDARD;
use base64::read::DecoderReader;
use serde_json::Value;

/// Validates media before model resolution or upstream calls.
pub(in crate::provider::antigravity) fn validate_media(
    messages: &[ChatMessage],
) -> Result<(), String> {
    for message in messages {
        if let Some(MessageContent::Parts(parts)) = &message.content {
            for part in parts {
                if let Some((_, data)) = inline_media(part)? {
                    if message.role != "user" {
                        return Err(format!(
                            "Media is not supported in {} messages",
                            message.role
                        ));
                    }
                    if data.is_empty()
                        || std::io::copy(
                            &mut DecoderReader::new(data.as_bytes(), &STANDARD),
                            &mut std::io::sink(),
                        )
                        .is_err()
                    {
                        return Err("Media data must be nonempty valid base64".into());
                    }
                }
            }
        }
    }
    Ok(())
}

/// Identifies supported inline media without fetching URLs or copying encoded bytes.
pub(super) fn inline_media(part: &ContentPart) -> Result<Option<(&str, &str)>, String> {
    let (mime, data) = match part {
        ContentPart::ImageUrl { image_url } => data_url(&image_url.url, "image/")?,
        ContentPart::InputImage { image_url } => data_url(
            image_url
                .as_deref()
                .ok_or("input_image requires an inline image_url")?,
            "image/",
        )?,
        ContentPart::InputAudio { fields } => {
            let audio = fields
                .get("input_audio")
                .and_then(Value::as_object)
                .ok_or("input_audio requires an input_audio object")?;
            let data = audio
                .get("data")
                .and_then(Value::as_str)
                .ok_or("input_audio requires base64 data")?;
            let mime = match audio.get("format").and_then(Value::as_str) {
                Some("wav") => "audio/wav",
                Some("mp3") => "audio/mpeg",
                _ => return Err("input_audio format must be wav or mp3".into()),
            };
            (mime, data)
        }
        ContentPart::File { fields } => {
            let file = fields
                .get("file")
                .and_then(Value::as_object)
                .ok_or("file requires a file object")?;
            if file.contains_key("file_id") {
                return Err("file IDs are not supported; provide inline PDF file_data".into());
            }
            data_url(
                file.get("file_data")
                    .and_then(Value::as_str)
                    .ok_or("file requires inline PDF file_data")?,
                "application/pdf",
            )?
        }
        ContentPart::InputFile { fields } => {
            if fields.contains_key("file_id") || fields.contains_key("file_url") {
                return Err(
                    "file IDs and URLs are not supported; provide inline PDF file_data".into(),
                );
            }
            data_url(
                fields
                    .get("file_data")
                    .and_then(Value::as_str)
                    .ok_or("input_file requires inline PDF file_data")?,
                "application/pdf",
            )?
        }
        ContentPart::VideoUrl { fields } | ContentPart::InputVideo { fields } => {
            let value = fields
                .get("video_url")
                .or_else(|| fields.get("url"))
                .ok_or("video requires an inline video_url")?;
            let url = value
                .as_str()
                .or_else(|| value.get("url").and_then(Value::as_str))
                .ok_or("video_url must be a data URL or an object containing url")?;
            data_url(url, "video/")?
        }
        _ => return Ok(None),
    };
    Ok(Some((mime, data)))
}

/// Accepts only explicit base64 data URLs with a matching MIME type.
fn data_url<'a>(url: &'a str, expected: &str) -> Result<(&'a str, &'a str), String> {
    let (mime, data) = parse_data_url(url)
        .ok_or("Media must be an inline base64 data URL; external URLs are not supported")?;
    let matches = if expected.ends_with('/') {
        mime.strip_prefix(expected).is_some_and(|subtype| {
            !subtype.is_empty()
                && subtype
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b".+-".contains(&b))
        })
    } else {
        mime == expected
    };
    if !matches {
        return Err(format!("Expected {expected} media MIME type"));
    }
    Ok((mime, data))
}

/// Splits a base64 data URL into its MIME type and encoded data.
pub(super) fn parse_data_url(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("data:")?;
    let (mime, after_semi) = rest.split_once(';')?;
    after_semi.strip_prefix("base64,").map(|data| (mime, data))
}
