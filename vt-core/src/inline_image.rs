//! OSC 1337 image 전송 형식의 엄격한 parsing이다.

use base64::Engine as _;
use std::collections::BTreeMap;

pub const MAX_IMAGE_BYTES: usize = 1_048_576;
const MAX_ENCODED_BYTES: usize = 1_398_104;
const MAX_NAME_BYTES: usize = 255;
const MAX_DIMENSION: u32 = 100_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dimension {
    Auto,
    Cells(u32),
    Pixels(u32),
    Percent(u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InlineImageCommand {
    Display {
        name: String,
        data: Vec<u8>,
        width: Dimension,
        height: Dimension,
        preserve_aspect_ratio: bool,
    },
    Transfer {
        name: Option<String>,
        data: Vec<u8>,
    },
    MultipartStart {
        name: String,
    },
    MultipartPart(Vec<u8>),
    MultipartEnd,
}

pub fn parse(payload: &[u8]) -> Result<InlineImageCommand, String> {
    if payload == b"FileEnd" {
        return Ok(InlineImageCommand::MultipartEnd);
    }
    if let Some(encoded) = payload.strip_prefix(b"FilePart:") {
        let encoded = std::str::from_utf8(encoded)
            .map_err(|_| "OSC 1337 image data is not ASCII base64".to_string())?;
        return Ok(InlineImageCommand::MultipartPart(decode_data(encoded)?));
    }
    if let Some(attributes) = payload.strip_prefix(b"MultipartFile=") {
        let attributes = std::str::from_utf8(attributes)
            .map_err(|_| "OSC 1337 multipart header is not UTF-8".to_string())?;
        let values = parse_attributes(attributes)?;
        if values.get("inline").map(String::as_str) != Some("1") {
            return Err("MultipartFile requires inline=1".into());
        }
        return Ok(InlineImageCommand::MultipartStart {
            name: decode_name(required(&values, "name")?)?,
        });
    }

    let (kind, attribute_text, encoded) = split_transfer(payload)?;
    let values = parse_attributes(attribute_text)?;
    match kind {
        "MultipartFile" => {
            if values.get("inline").map(String::as_str) != Some("1") {
                return Err("MultipartFile requires inline=1".into());
            }
            let name = decode_name(required(&values, "name")?)?;
            if !encoded.is_empty() {
                return Err("MultipartFile must not contain image data".into());
            }
            Ok(InlineImageCommand::MultipartStart { name })
        }
        "File" => {
            let data = decode_data(encoded)?;
            if let Some(size) = values.get("size") {
                let declared = size
                    .parse::<usize>()
                    .map_err(|_| "image size must be a non-negative integer".to_string())?;
                if declared != data.len() {
                    return Err(format!(
                        "image size {declared} does not match decoded payload length {}",
                        data.len()
                    ));
                }
            }
            let name = values
                .get("name")
                .map(|value| decode_name(value))
                .transpose()?;
            let inline = match values.get("inline") {
                None => false,
                Some(value) if value == "0" => false,
                Some(value) if value == "1" => true,
                Some(_) => return Err("inline must be 0 or 1".into()),
            };
            if !inline {
                return Ok(InlineImageCommand::Transfer { name, data });
            }
            let name = name.ok_or_else(|| "inline image name is required".to_string())?;
            Ok(InlineImageCommand::Display {
                name,
                data,
                width: dimension(values.get("width"))?,
                height: dimension(values.get("height"))?,
                preserve_aspect_ratio: boolean(&values, "preserveAspectRatio", true)?,
            })
        }
        _ => Err(format!("unsupported OSC 1337 transfer {kind}")),
    }
}

fn split_transfer(payload: &[u8]) -> Result<(&str, &str, &str), String> {
    let separator = payload
        .iter()
        .position(|byte| *byte == b':')
        .ok_or_else(|| "OSC 1337 transfer is missing its data separator".to_string())?;
    let header = std::str::from_utf8(&payload[..separator])
        .map_err(|_| "OSC 1337 transfer header is not UTF-8".to_string())?;
    let (kind, attributes) = if let Some(attributes) = header.strip_prefix("File=") {
        ("File", attributes)
    } else if let Some(attributes) = header.strip_prefix("MultipartFile=") {
        ("MultipartFile", attributes)
    } else {
        return Err("OSC 1337 transfer is missing File or MultipartFile".into());
    };
    let encoded = std::str::from_utf8(&payload[separator + 1..])
        .map_err(|_| "OSC 1337 image data is not ASCII base64".to_string())?;
    if encoded.len() > MAX_ENCODED_BYTES {
        return Err(format!(
            "OSC 1337 image payload exceeds {MAX_IMAGE_BYTES} bytes"
        ));
    }
    Ok((kind, attributes, encoded))
}

fn parse_attributes(text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut values = BTreeMap::new();
    if text.is_empty() {
        return Ok(values);
    }
    for item in text.split(';') {
        let (key, value) = item
            .split_once('=')
            .ok_or_else(|| format!("OSC 1337 attribute {item} is missing a value"))?;
        if key.is_empty() || value.is_empty() || values.insert(key.into(), value.into()).is_some() {
            return Err(format!("invalid or duplicate OSC 1337 attribute {item}"));
        }
    }
    Ok(values)
}

fn required<'a>(values: &'a BTreeMap<String, String>, key: &str) -> Result<&'a str, String> {
    values
        .get(key)
        .map(String::as_str)
        .ok_or_else(|| format!("OSC 1337 {key} is required"))
}

fn decode_name(encoded: &str) -> Result<String, String> {
    let bytes = decode_base64(encoded.as_bytes(), "image name")?;
    if bytes.is_empty() || bytes.len() > MAX_NAME_BYTES {
        return Err("image name is empty or too long".into());
    }
    String::from_utf8(bytes).map_err(|_| "image name is not UTF-8".into())
}

fn decode_data(encoded: &str) -> Result<Vec<u8>, String> {
    decode_base64(encoded.as_bytes(), "image data")
}

fn decode_base64(encoded: &[u8], label: &str) -> Result<Vec<u8>, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|error| format!("invalid base64 {label}: {error}"))?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!("{label} exceeds {MAX_IMAGE_BYTES} bytes"));
    }
    Ok(bytes)
}

fn boolean(values: &BTreeMap<String, String>, key: &str, default: bool) -> Result<bool, String> {
    match values.get(key) {
        None => Ok(default),
        Some(value) if value == "0" => Ok(false),
        Some(value) if value == "1" => Ok(true),
        Some(_) => Err(format!("{key} must be 0 or 1")),
    }
}

fn dimension(value: Option<&String>) -> Result<Dimension, String> {
    let Some(value) = value else {
        return Ok(Dimension::Auto);
    };
    if value == "auto" {
        return Ok(Dimension::Auto);
    }
    if let Some(number) = value.strip_suffix("px") {
        return number
            .parse::<u32>()
            .ok()
            .filter(|number| *number > 0 && *number <= MAX_DIMENSION)
            .map(Dimension::Pixels)
            .ok_or_else(|| format!("invalid pixel dimension {value}"));
    }
    if let Some(number) = value.strip_suffix('%') {
        return number
            .parse::<u8>()
            .ok()
            .filter(|number| *number > 0 && *number <= 100)
            .map(Dimension::Percent)
            .ok_or_else(|| format!("invalid percentage dimension {value}"));
    }
    value
        .parse::<u32>()
        .ok()
        .filter(|number| *number > 0 && *number <= MAX_DIMENSION)
        .map(Dimension::Cells)
        .ok_or_else(|| format!("invalid cell dimension {value}"))
}
