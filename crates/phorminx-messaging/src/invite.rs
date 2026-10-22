//! Onion addresses and the shareable invite string `name#<base32>`.
use crate::{MessagingError, base32, identity::sha256};

/// A v3 onion service address (56 base32 characters encoding the service key,
/// a checksum and the version byte 3).
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OnionAddress(String);

impl std::fmt::Debug for OnionAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Addresses are private to contacts; keep them out of logs.
        write!(f, "OnionAddress({}..)", &self.0[..6])
    }
}

impl OnionAddress {
    const RAW_LEN: usize = 35;

    pub fn parse(text: &str) -> Option<Self> {
        let host = text.strip_suffix(".onion").unwrap_or(text);
        if host.len() != 56 {
            return None;
        }
        let raw = base32::decode(host)?;
        Self::from_raw(&raw)
    }

    pub(crate) fn from_raw(raw: &[u8]) -> Option<Self> {
        (raw.len() == Self::RAW_LEN && raw[34] == 3).then(|| Self(base32::encode(raw)))
    }

    pub(crate) fn raw(&self) -> Vec<u8> {
        base32::decode(&self.0).expect("validated at construction")
    }

    /// The address with the `.onion` suffix, as Tor expects it.
    pub fn as_onion(&self) -> String {
        format!("{}.onion", self.0)
    }
}

impl std::fmt::Display for OnionAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.onion", self.0)
    }
}

pub const INVITE_VERSION: u8 = 1;
pub const INVITE_SECRET_LEN: usize = 16;
const PAYLOAD_LEN: usize = 1 + OnionAddress::RAW_LEN + INVITE_SECRET_LEN + 8 + 4;
const MAX_NAME_CHARS: usize = 64;

/// A one-time invite. The name is only a hint for the person pasting it; the
/// contact's real display name comes from the handshake.
#[derive(Clone, PartialEq, Eq)]
pub struct Invite {
    pub name: String,
    pub address: OnionAddress,
    pub secret: [u8; INVITE_SECRET_LEN],
    /// Unix seconds (UTC).
    pub expires_at: u64,
}

impl std::fmt::Debug for Invite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Invite")
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

impl Invite {
    pub fn encode(&self) -> String {
        let mut payload = Vec::with_capacity(PAYLOAD_LEN);
        payload.push(INVITE_VERSION);
        payload.extend_from_slice(&self.address.raw());
        payload.extend_from_slice(&self.secret);
        payload.extend_from_slice(&self.expires_at.to_be_bytes());
        let check = sha256(&payload);
        payload.extend_from_slice(&check[..4]);
        format!("{}#{}", invite_name(&self.name), base32::encode(&payload))
    }

    pub fn decode(text: &str) -> Result<Self, MessagingError> {
        let text = text.trim();
        let (name, code) = text.rsplit_once('#').ok_or(MessagingError::InvalidInvite)?;
        let payload = base32::decode(code.trim()).ok_or(MessagingError::InvalidInvite)?;
        if payload.len() != PAYLOAD_LEN {
            return Err(MessagingError::InvalidInvite);
        }
        let (body, check) = payload.split_at(PAYLOAD_LEN - 4);
        if sha256(body)[..4] != *check {
            return Err(MessagingError::InvalidInvite);
        }
        if body[0] != INVITE_VERSION {
            return Err(MessagingError::UnsupportedInvite);
        }
        let address = OnionAddress::from_raw(&body[1..1 + OnionAddress::RAW_LEN])
            .ok_or(MessagingError::InvalidInvite)?;
        let rest = &body[1 + OnionAddress::RAW_LEN..];
        let secret = rest[..INVITE_SECRET_LEN].try_into().expect("fixed length");
        let expires_at = u64::from_be_bytes(rest[INVITE_SECRET_LEN..].try_into().expect("8 bytes"));
        Ok(Self {
            name: invite_name(name),
            address,
            secret,
            expires_at,
        })
    }
}

/// Keeps the visible name part short and printable; never empty.
fn invite_name(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .filter(|c| !c.is_control() && *c != '#')
        .take(MAX_NAME_CHARS)
        .collect();
    if cleaned.trim().is_empty() {
        "contact".into()
    } else {
        cleaned.trim().to_string()
    }
}

#[cfg(test)]
pub(crate) fn test_address(seed: u8) -> OnionAddress {
    let mut raw = [seed; OnionAddress::RAW_LEN];
    raw[34] = 3;
    OnionAddress::from_raw(&raw).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invite() -> Invite {
        Invite {
            name: "mau".into(),
            address: test_address(9),
            secret: [5; INVITE_SECRET_LEN],
            expires_at: 1_800_000_000,
        }
    }

    #[test]
    fn roundtrip() {
        let text = invite().encode();
        assert!(text.starts_with("mau#"));
        assert_eq!(Invite::decode(&text).unwrap(), invite());
        assert_eq!(
            Invite::decode(&format!("  {}\n", text.to_uppercase()))
                .unwrap()
                .secret,
            [5; 16]
        );
    }

    #[test]
    fn name_with_hash_and_controls_is_cleaned() {
        let mut i = invite();
        i.name = "a#b\nc".into();
        let text = i.encode();
        assert!(text.starts_with("abc#"));
        assert_eq!(Invite::decode(&text).unwrap().name, "abc");
        i.name = "  ".into();
        assert!(i.encode().starts_with("contact#"));
    }

    #[test]
    fn typos_are_detected() {
        let text = invite().encode();
        let (name, code) = text.split_once('#').unwrap();
        let mut chars: Vec<char> = code.chars().collect();
        chars[10] = if chars[10] == 'a' { 'b' } else { 'a' };
        let broken = format!("{name}#{}", chars.into_iter().collect::<String>());
        assert!(matches!(
            Invite::decode(&broken),
            Err(MessagingError::InvalidInvite)
        ));
        assert!(Invite::decode("no-hash").is_err());
        assert!(Invite::decode("x#").is_err());
        assert!(Invite::decode(&format!("{text}a")).is_err());
    }

    #[test]
    fn address_parsing() {
        let a = test_address(1);
        assert_eq!(OnionAddress::parse(&a.to_string()), Some(a.clone()));
        assert_eq!(
            OnionAddress::parse(&a.as_onion().to_uppercase().replace(".ONION", "")),
            Some(a.clone())
        );
        assert!(OnionAddress::parse("short.onion").is_none());
        let mut raw = a.raw();
        raw[34] = 2;
        assert!(OnionAddress::from_raw(&raw).is_none());
        assert!(!format!("{a:?}").contains(&a.as_onion()));
    }

    #[test]
    fn real_tor_address_parses() {
        let addr = "2yvjkouwysb2w54kmxqyslltccsuiqcwfotiaaln7abpwjml2y5u7dqd.onion";
        assert_eq!(OnionAddress::parse(addr).unwrap().to_string(), addr);
    }
}
