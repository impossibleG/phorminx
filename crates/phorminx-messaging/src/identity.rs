//! Ed25519 identity keys and the handshake proof.
use ring::{
    rand::{SecureRandom, SystemRandom},
    signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey},
};

use crate::MessagingError;

const PROOF_DOMAIN: &[u8] = b"phorminx-messaging/v1/proof\0";

/// Secret bytes that are overwritten when dropped. Debug output is redacted.
pub struct SecretBytes(Vec<u8>);

impl SecretBytes {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for SecretBytes {
    fn drop(&mut self) {
        for byte in &mut self.0 {
            // Volatile writes keep the compiler from eliding the clearing.
            unsafe { std::ptr::write_volatile(byte, 0) };
        }
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}

impl std::fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretBytes([redacted])")
    }
}

/// An Ed25519 public identity key.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PublicKey(pub [u8; 32]);

impl std::fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PublicKey({:02x}{:02x}..)", self.0[0], self.0[1])
    }
}

impl PublicKey {
    pub fn from_slice(bytes: &[u8]) -> Option<Self> {
        bytes.try_into().ok().map(Self)
    }

    pub fn verify_proof(&self, challenge: &[u8], server_address: &str, signature: &[u8]) -> bool {
        UnparsedPublicKey::new(&ED25519, self.0)
            .verify(&proof_message(challenge, server_address, self), signature)
            .is_ok()
    }
}

/// The local messaging identity. The seed never leaves this type except to be
/// protected for storage.
pub struct Identity {
    pair: Ed25519KeyPair,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Identity").field(&self.public_key()).finish()
    }
}

impl Identity {
    /// Generates a fresh identity and returns it with its seed for storage.
    pub fn generate() -> Result<(Self, SecretBytes), MessagingError> {
        let seed = SecretBytes::new(random_bytes::<32>()?.to_vec());
        Ok((Self::from_seed(seed.as_bytes())?, seed))
    }

    pub fn from_seed(seed: &[u8]) -> Result<Self, MessagingError> {
        Ed25519KeyPair::from_seed_unchecked(seed)
            .map(|pair| Self { pair })
            .map_err(|_| MessagingError::Identity)
    }

    pub fn public_key(&self) -> PublicKey {
        let mut key = [0; 32];
        key.copy_from_slice(self.pair.public_key().as_ref());
        PublicKey(key)
    }

    /// Signs a server's challenge, bound to the address we dialed so a proof
    /// cannot be relayed to a different service.
    pub fn prove(&self, challenge: &[u8], server_address: &str) -> Vec<u8> {
        self.pair
            .sign(&proof_message(
                challenge,
                server_address,
                &self.public_key(),
            ))
            .as_ref()
            .to_vec()
    }
}

fn proof_message(challenge: &[u8], server_address: &str, client: &PublicKey) -> Vec<u8> {
    let mut message = Vec::with_capacity(PROOF_DOMAIN.len() + challenge.len() + 128);
    message.extend_from_slice(PROOF_DOMAIN);
    message.extend_from_slice(&(challenge.len() as u32).to_be_bytes());
    message.extend_from_slice(challenge);
    message.extend_from_slice(&(server_address.len() as u32).to_be_bytes());
    message.extend_from_slice(server_address.as_bytes());
    message.extend_from_slice(&client.0);
    message
}

pub(crate) fn random_bytes<const N: usize>() -> Result<[u8; N], MessagingError> {
    let mut bytes = [0; N];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| MessagingError::Identity)?;
    Ok(bytes)
}

pub(crate) fn sha256(bytes: &[u8]) -> [u8; 32] {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let mut out = [0; 32];
    out.copy_from_slice(digest.as_ref());
    out
}

/// Constant-time comparison for secrets.
pub(crate) fn same_secret(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A random version-4 UUID string for message ids.
pub(crate) fn new_uuid() -> Result<String, MessagingError> {
    let mut b = random_bytes::<16>()?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proof_binds_challenge_address_and_key() {
        let (a, seed) = Identity::generate().unwrap();
        let restored = Identity::from_seed(seed.as_bytes()).unwrap();
        assert_eq!(a.public_key(), restored.public_key());
        let (b, _) = Identity::generate().unwrap();
        let proof = a.prove(b"challenge", "x.onion");
        assert!(a.public_key().verify_proof(b"challenge", "x.onion", &proof));
        assert!(!a.public_key().verify_proof(b"challengf", "x.onion", &proof));
        assert!(!a.public_key().verify_proof(b"challenge", "y.onion", &proof));
        assert!(!b.public_key().verify_proof(b"challenge", "x.onion", &proof));
        assert_eq!(format!("{seed:?}"), "SecretBytes([redacted])");
    }

    #[test]
    fn uuids_are_v4_shaped() {
        let id = new_uuid().unwrap();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert_ne!(id, new_uuid().unwrap());
    }

    #[test]
    fn constant_time_compare() {
        assert!(same_secret(b"abc", b"abc"));
        assert!(!same_secret(b"abc", b"abd"));
        assert!(!same_secret(b"abc", b"ab"));
    }
}
