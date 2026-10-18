//! Wire protocol v1: a 4-byte big-endian length followed by a JSON frame.
//!
//! Frames reject unknown fields and carry no identifying metadata (no locale,
//! host name, paths or app version). Times are UTC milliseconds since the Unix
//! epoch.
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::invite::OnionAddress;

pub const PROTOCOL_VERSION: u32 = 1;
/// Largest encoded frame accepted, in bytes (excluding the length prefix).
pub const MAX_FRAME_BYTES: usize = 32 * 1024;
/// Largest message body, in bytes. JSON escaping stays within the frame limit
/// for any printable text of this size.
pub const MAX_TEXT_BYTES: usize = 4 * 1024;
pub const MAX_NAME_CHARS: usize = 64;
pub const CHALLENGE_BYTES: usize = 32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Frame {
    Hello {
        version: u32,
        identity_key: String,
        display_name: String,
    },
    Challenge {
        nonce: String,
    },
    Proof {
        signature: String,
    },
    RedeemInvite {
        secret: String,
        contacts_address: String,
        /// Lets the inviter find our contacts service when it uses restricted
        /// discovery. Absent when the service is open.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        discovery_key: Option<String>,
    },
    InviteAccepted {
        contacts_address: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        discovery_key: Option<String>,
    },
    InviteRejected {},
    Message {
        id: String,
        sent_at: i64,
        text: String,
    },
    Ack {
        id: String,
    },
    Goodbye {},
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("connection closed")]
    Closed,
    #[error("connection failed")]
    Io(#[from] std::io::Error),
    #[error("frame too large")]
    TooLarge,
    #[error("malformed frame")]
    Malformed,
    #[error("unexpected frame")]
    Unexpected,
}

impl Frame {
    pub fn hello(identity_key: &[u8; 32], display_name: &str) -> Self {
        Self::Hello {
            version: PROTOCOL_VERSION,
            identity_key: b64(identity_key),
            display_name: display_name.to_string(),
        }
    }

    /// Checks every field against its limits. Called on decode, so a frame
    /// that reaches the session logic is always well-formed.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let ok = match self {
            Self::Hello {
                version,
                identity_key,
                display_name,
            } => *version >= 1 && key32(identity_key).is_some() && valid_name(display_name),
            Self::Challenge { nonce } => unb64(nonce).is_some_and(|n| n.len() == CHALLENGE_BYTES),
            Self::Proof { signature } => unb64(signature).is_some_and(|s| s.len() == 64),
            Self::RedeemInvite {
                secret,
                contacts_address,
                discovery_key,
            } => {
                unb64(secret).is_some_and(|s| s.len() == crate::invite::INVITE_SECRET_LEN)
                    && OnionAddress::parse(contacts_address).is_some()
                    && discovery_key.as_deref().is_none_or(|k| key32(k).is_some())
            }
            Self::InviteAccepted {
                contacts_address,
                discovery_key,
            } => {
                OnionAddress::parse(contacts_address).is_some()
                    && discovery_key.as_deref().is_none_or(|k| key32(k).is_some())
            }
            Self::Message { id, sent_at, text } => {
                valid_uuid(id) && *sent_at >= 0 && valid_text(text)
            }
            Self::Ack { id } => valid_uuid(id),
            Self::InviteRejected {} | Self::Goodbye {} => true,
        };
        ok.then_some(()).ok_or(ProtocolError::Malformed)
    }

    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        self.validate()?;
        let body = serde_json::to_vec(self).map_err(|_| ProtocolError::Malformed)?;
        if body.len() > MAX_FRAME_BYTES {
            return Err(ProtocolError::TooLarge);
        }
        let mut out = Vec::with_capacity(4 + body.len());
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(&body);
        Ok(out)
    }

    pub fn decode(body: &[u8]) -> Result<Self, ProtocolError> {
        if body.len() > MAX_FRAME_BYTES {
            return Err(ProtocolError::TooLarge);
        }
        let frame: Self = serde_json::from_slice(body).map_err(|_| ProtocolError::Malformed)?;
        frame.validate()?;
        Ok(frame)
    }
}

pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Frame, ProtocolError> {
    let mut len = [0u8; 4];
    match reader.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(ProtocolError::Closed);
        }
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_be_bytes(len) as usize;
    if len == 0 {
        return Err(ProtocolError::Malformed);
    }
    if len > MAX_FRAME_BYTES {
        return Err(ProtocolError::TooLarge);
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            ProtocolError::Closed
        } else {
            e.into()
        }
    })?;
    Frame::decode(&body)
}

pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &Frame,
) -> Result<(), ProtocolError> {
    writer.write_all(&frame.encode()?).await?;
    writer.flush().await?;
    Ok(())
}

pub fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

pub fn unb64(text: &str) -> Option<Vec<u8>> {
    if text.len() > 256 {
        return None;
    }
    URL_SAFE_NO_PAD.decode(text).ok()
}

pub fn key32(text: &str) -> Option<[u8; 32]> {
    unb64(text)?.try_into().ok()
}

pub fn valid_name(name: &str) -> bool {
    let count = name.chars().count();
    count > 0
        && count <= MAX_NAME_CHARS
        && !name.trim().is_empty()
        && !name.chars().any(char::is_control)
}

pub fn valid_text(text: &str) -> bool {
    !text.trim().is_empty() && text.len() <= MAX_TEXT_BYTES
}

fn valid_uuid(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => b == b'-',
            _ => b.is_ascii_hexdigit() && !b.is_ascii_uppercase(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invite::test_address;

    const ID: &str = "0a1b2c3d-0000-4000-8000-000000000001";

    fn samples() -> Vec<Frame> {
        vec![
            Frame::hello(&[1; 32], "Ana"),
            Frame::Challenge {
                nonce: b64(&[2; 32]),
            },
            Frame::Proof {
                signature: b64(&[3; 64]),
            },
            Frame::RedeemInvite {
                secret: b64(&[4; 16]),
                contacts_address: test_address(1).to_string(),
                discovery_key: Some(b64(&[5; 32])),
            },
            Frame::RedeemInvite {
                secret: b64(&[4; 16]),
                contacts_address: test_address(1).to_string(),
                discovery_key: None,
            },
            Frame::InviteAccepted {
                contacts_address: test_address(2).to_string(),
                discovery_key: None,
            },
            Frame::InviteRejected {},
            Frame::Message {
                id: ID.into(),
                sent_at: 1_700_000_000_000,
                text: "olá \"mundo\"\n".into(),
            },
            Frame::Ack { id: ID.into() },
            Frame::Goodbye {},
        ]
    }

    #[tokio::test]
    async fn every_frame_roundtrips_over_a_stream() {
        let (mut a, mut b) = tokio::io::duplex(1 << 16);
        for frame in samples() {
            write_frame(&mut a, &frame).await.unwrap();
            assert_eq!(read_frame(&mut b).await.unwrap(), frame);
        }
        drop(a);
        assert!(matches!(
            read_frame(&mut b).await,
            Err(ProtocolError::Closed)
        ));
    }

    #[test]
    fn rejects_unknown_fields_and_types() {
        for json in [
            r#"{"type":"goodbye","x":1}"#,
            r#"{"type":"ack","id":"0a1b2c3d-0000-4000-8000-000000000001","locale":"pt-BR"}"#,
            r#"{"type":"teleport"}"#,
            r#"{"id":"0a1b2c3d-0000-4000-8000-000000000001"}"#,
            r#"{"type":"hello","version":1,"identity_key":"AAAA","display_name":"x"}"#,
            r#"{"type":"message","id":"nope","sent_at":1,"text":"x"}"#,
            r#"{"type":"message","id":"0a1b2c3d-0000-4000-8000-000000000001","sent_at":-1,"text":"x"}"#,
            r#"{"type":"message","id":"0a1b2c3d-0000-4000-8000-000000000001","sent_at":1,"text":"  "}"#,
            "not json",
        ] {
            assert!(Frame::decode(json.as_bytes()).is_err(), "{json}");
        }
        assert!(Frame::decode(br#"{"type":"goodbye"}"#).is_ok());
    }

    #[test]
    fn enforces_size_limits() {
        let big = Frame::Message {
            id: ID.into(),
            sent_at: 1,
            text: "x".repeat(MAX_TEXT_BYTES + 1),
        };
        assert!(big.encode().is_err());
        let max = Frame::Message {
            id: ID.into(),
            sent_at: 1,
            // Control characters are the worst case for JSON escaping.
            text: "\u{1}".repeat(MAX_TEXT_BYTES),
        };
        assert!(max.encode().unwrap().len() <= MAX_FRAME_BYTES + 4);
        assert!(!valid_name(&"n".repeat(MAX_NAME_CHARS + 1)));
        assert!(!valid_name("a\u{7}b"));
    }

    #[tokio::test]
    async fn rejects_oversized_or_empty_length_prefix() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&((MAX_FRAME_BYTES as u32) + 1).to_be_bytes())
            .await
            .unwrap();
        assert!(matches!(
            read_frame(&mut b).await,
            Err(ProtocolError::TooLarge)
        ));
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&0u32.to_be_bytes()).await.unwrap();
        assert!(matches!(
            read_frame(&mut b).await,
            Err(ProtocolError::Malformed)
        ));
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&10u32.to_be_bytes()).await.unwrap();
        a.write_all(b"{\"ty").await.unwrap();
        drop(a);
        assert!(matches!(
            read_frame(&mut b).await,
            Err(ProtocolError::Closed)
        ));
    }

    #[test]
    fn frames_carry_no_extra_metadata() {
        let json = String::from_utf8(Frame::hello(&[1; 32], "Ana").encode().unwrap()[4..].to_vec())
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        let keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["display_name", "identity_key", "type", "version"]);
    }
}
