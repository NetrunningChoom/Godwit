// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Array-form protobuf as documented by maugclib/pblite.py. Wire metadata
//! is generated from googlechat.proto; generated messages keep lite_runtime.
use anyhow::{Context, Result, bail};
use base64::Engine;
use serde_json::Value;
include!(concat!(env!("OUT_DIR"), "/pblite_fields.rs"));

pub fn decode<M: protobuf::Message>(message: &mut M, name: &str, value: &Value) -> Result<()> {
    let mut bytes = Vec::new();
    encode(name, value, &mut bytes, 0)?;
    message.merge_from_bytes(&bytes).context("invalid Google Chat protobuf")
}
fn varint(mut n: u64, bytes: &mut Vec<u8>) {
    while n > 127 {
        bytes.push((n as u8 & 127) | 128);
        n >>= 7;
    }
    bytes.push(n as u8);
}
fn encode(name: &str, value: &Value, bytes: &mut Vec<u8>, depth: usize) -> Result<()> {
    if depth > 64 {
        bail!("Google Chat event nesting exceeds the limit");
    }
    let array = value.as_array().context("Google Chat event is not an array")?;
    let extras = array.last().and_then(Value::as_object);
    for &(tag, ty, nested, repeated) in fields(name) {
        let value = extras
            .and_then(|e| e.get(&tag.to_string()))
            .or_else(|| array.get(tag as usize - 1));
        let Some(value) = value.filter(|v| !v.is_null() && !v.is_object()) else {
            continue;
        };
        if repeated {
            for v in value.as_array().context("invalid Google Chat repeated field")? {
                scalar(tag, ty, nested, v, bytes, depth)?;
            }
        } else {
            scalar(tag, ty, nested, value, bytes, depth)?;
        }
        if bytes.len() > 16 * 1024 * 1024 {
            bail!("Google Chat event exceeds the limit");
        }
    }
    Ok(())
}
fn scalar(tag: u32, ty: i32, nested: &str, v: &Value, bytes: &mut Vec<u8>, depth: usize) -> Result<()> {
    let int = || v.as_i64().or_else(|| v.as_str()?.parse().ok());
    let uint = || v.as_u64().or_else(|| v.as_str()?.parse().ok());
    let fixed = match ty {
        1 => v.as_f64().map(|n| (1, n.to_le_bytes().to_vec())),
        2 => v.as_f64().map(|n| (5, (n as f32).to_le_bytes().to_vec())),
        6 => uint().map(|n| (1, n.to_le_bytes().to_vec())),
        7 => uint()
            .and_then(|n| u32::try_from(n).ok())
            .map(|n| (5, n.to_le_bytes().to_vec())),
        15 => int()
            .and_then(|n| i32::try_from(n).ok())
            .map(|n| (5, n.to_le_bytes().to_vec())),
        16 => int().map(|n| (1, n.to_le_bytes().to_vec())),
        _ => None,
    };
    if let Some((wire, data)) = fixed {
        varint(u64::from(tag) << 3 | wire, bytes);
        bytes.extend(data);
        return Ok(());
    }
    let data = match ty {
        9 => v.as_str().map(|s| s.as_bytes().to_vec()),
        12 => v
            .as_str()
            .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok()),
        11 => {
            let mut data = Vec::new();
            encode(nested, v, &mut data, depth + 1)?;
            Some(data)
        }
        _ => None,
    };
    if let Some(data) = data {
        varint(u64::from(tag) << 3 | 2, bytes);
        varint(data.len() as u64, bytes);
        bytes.extend(data);
        return Ok(());
    }
    let n = match ty {
        3 | 5 | 14 => int().map(|n| n as u64),
        4 | 13 => uint(),
        8 => v.as_bool().map(u64::from).or_else(|| uint().filter(|n| *n <= 1)),
        17 | 18 => int().map(|n| ((n << 1) ^ (n >> 63)) as u64),
        _ => None,
    };
    if let Some(n) = n {
        varint(u64::from(tag) << 3, bytes);
        varint(n, bytes);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto;
    use protobuf::Enum;
    use serde_json::json;
    #[test]
    fn stream_fields_and_sparse_high_tags_decode() {
        let mut response = proto::StreamEventsResponse::new();
        // StreamEventsResponse.event -> Event.group_id + type; user_revision at field 6.
        decode(
            &mut response,
            ".StreamEventsResponse",
            &json!([[null,null,6,null,null,["1234567890123456"], {"999": [1,2]}]]),
        )
        .unwrap();
        assert_eq!(response.event.type_().value(), 6);
        let mut message = proto::MessageEvent::new();
        decode(
            &mut message,
            ".MessageEvent",
            &json!([null, null, null, "9007199254740993"]),
        )
        .unwrap();
        assert_eq!(message.last_message_in_topic_time(), 9007199254740993);
    }
    #[test]
    fn malformed_values_fail_without_panicking() {
        assert!(
            decode(
                &mut proto::StreamEventsResponse::new(),
                ".StreamEventsResponse",
                &json!({})
            )
            .is_err()
        );
        assert!(
            decode(
                &mut proto::StreamEventsResponse::new(),
                ".StreamEventsResponse",
                &json!([42])
            )
            .is_err()
        );
    }
}
