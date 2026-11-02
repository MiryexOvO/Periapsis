//! Bounded JSON application controls, compatible with the TypeScript reference.
//! 有大小限制的 JSON 应用层控制消息，格式与 TypeScript 参考端一致。

use crate::{handshake::Message, validate};
use nsp_types::{Agreement, Hello, MAX_CONTROL_BYTES};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

#[derive(Debug)]
pub struct WireError(String);
impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for WireError {}

/// Decode a bounded control object before passing it to the handshake.
/// 在控制对象进入状态机前，校验大小、会话标识和嵌套能力声明。
pub fn decode(bytes: &[u8]) -> Result<Message, WireError> {
    if bytes.len() > MAX_CONTROL_BYTES {
        return Err(WireError("control message too large".into()));
    }
    let message: Message = serde_json::from_slice(bytes).map_err(|e| WireError(e.to_string()))?;
    let id = message.session_id();
    if id.len() != 32
        || !id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(WireError("invalid session id".into()));
    }
    match &message {
        Message::Offer { hello, .. } | Message::Accept { hello, .. } => {
            validate(hello).map_err(|e| WireError(e.to_string()))?
        }
        _ => {}
    }
    Ok(message)
}

/// Encode and validate an application control; MOQT remains responsible for framing.
/// 编码并校验应用层控制消息，帧封装仍由 MOQT 负责。
pub fn encode(message: &Message) -> Result<Vec<u8>, WireError> {
    let bytes = serde_json::to_vec(message).map_err(|e| WireError(e.to_string()))?;
    decode(&bytes)?;
    Ok(bytes)
}

// 嵌套结构包含固定类型标识；使用结构化派生避免手工拼接 JSON。
#[derive(Serialize, Deserialize)]
struct TaggedHello {
    #[serde(rename = "type")]
    kind: String,
    #[serde(flatten)]
    value: Hello,
}
#[derive(Serialize, Deserialize)]
struct TaggedAgreement {
    #[serde(rename = "type")]
    kind: String,
    #[serde(flatten)]
    value: Agreement,
}
pub(crate) mod hello {
    use super::*;
    pub fn serialize<S: Serializer>(value: &Hello, serializer: S) -> Result<S::Ok, S::Error> {
        TaggedHello {
            kind: "nsp.hello".into(),
            value: value.clone(),
        }
        .serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Hello, D::Error> {
        let tagged = TaggedHello::deserialize(deserializer)?;
        if tagged.kind != "nsp.hello" {
            return Err(serde::de::Error::custom("invalid hello type"));
        }
        Ok(tagged.value)
    }
}
pub(crate) mod agreement {
    use super::*;
    pub fn serialize<S: Serializer>(value: &Agreement, serializer: S) -> Result<S::Ok, S::Error> {
        TaggedAgreement {
            kind: "nsp.accept".into(),
            value: value.clone(),
        }
        .serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Agreement, D::Error> {
        let tagged = TaggedAgreement::deserialize(deserializer)?;
        if tagged.kind != "nsp.accept" {
            return Err(serde::de::Error::custom("invalid agreement type"));
        }
        Ok(tagged.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const ACK: &[u8] = br#"{"type":"nsp.ack","sessionId":"0123456789abcdef0123456789abcdef"}"#;
    #[test]
    fn controls_round_trip() {
        let message = decode(ACK).unwrap();
        assert_eq!(decode(&encode(&message).unwrap()).unwrap(), message);
    }
    #[test]
    fn malformed_and_oversized_controls_are_rejected() {
        assert!(decode(&vec![0; MAX_CONTROL_BYTES + 1]).is_err());
        assert!(decode(b"{}").is_err());
        assert!(decode(&[255]).is_err());
        assert!(decode(br#"{"type":"nsp.ack","sessionId":"bad"}"#).is_err());
    }
    #[test]
    fn nested_hello_requires_its_type_tag() {
        assert!(decode(br#"{"type":"nsp.offer","sessionId":"0123456789abcdef0123456789abcdef","hello":{"version":1,"profiles":["live"],"codecs":["application/octet-stream"],"extensions":[]}}"#).is_err());
    }
}
