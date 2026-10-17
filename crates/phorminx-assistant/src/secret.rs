use crate::AssistantError;
use serde::{Deserialize, Serialize};

/// Serialized bytes are account-bound Windows DPAPI ciphertext, never plaintext.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtectedSecret {
    ciphertext: Vec<u8>,
}
impl std::fmt::Debug for ProtectedSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProtectedSecret([redacted])")
    }
}
/// Short-lived plaintext. Debug is redacted and its allocation is overwritten on drop.
pub struct ExposedSecret(Vec<u8>);
impl ExposedSecret {
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("validated UTF-8")
    }
}
impl std::fmt::Debug for ExposedSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ExposedSecret([redacted])")
    }
}
impl Drop for ExposedSecret {
    fn drop(&mut self) {
        for byte in &mut self.0 {
            // Volatile writes keep the compiler from eliding secret-buffer clearing.
            unsafe {
                std::ptr::write_volatile(byte, 0);
            }
        }
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}
impl ProtectedSecret {
    pub fn protect(plaintext: &str) -> Result<Self, AssistantError> {
        if plaintext.is_empty()
            || plaintext.len() > 16_384
            || plaintext.chars().any(|c| c.is_control())
        {
            return Err(AssistantError::Credential);
        }
        Ok(Self {
            ciphertext: transform(plaintext.as_bytes(), true, ASSISTANT_ENTROPY)?,
        })
    }
    pub fn expose(&self) -> Result<ExposedSecret, AssistantError> {
        if self.ciphertext.is_empty() || self.ciphertext.len() > 65_536 {
            return Err(AssistantError::Credential);
        }
        let plain = ExposedSecret(transform(&self.ciphertext, false, ASSISTANT_ENTROPY)?);
        let text = std::str::from_utf8(&plain.0).map_err(|_| AssistantError::Credential)?;
        if text.is_empty() || text.len() > 16_384 || text.chars().any(|c| c.is_control()) {
            return Err(AssistantError::Credential);
        }
        Ok(plain)
    }
}

const ASSISTANT_ENTROPY: &[u8] = b"Phorminx/user-triggered-assistant/v1";
const MESSAGING_IDENTITY_ENTROPY: &[u8] = b"Phorminx/messaging-identity/v1";

/// Account-bound DPAPI ciphertext of a binary secret such as a private key.
/// Uses its own entropy tag, so it never decrypts as another secret kind.
#[derive(Clone, PartialEq, Eq)]
pub struct ProtectedBytes {
    ciphertext: Vec<u8>,
}
impl std::fmt::Debug for ProtectedBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ProtectedBytes([redacted])")
    }
}
impl ProtectedBytes {
    /// Protects a messaging identity key.
    pub fn protect_identity_key(plaintext: &[u8]) -> Result<Self, AssistantError> {
        if plaintext.is_empty() || plaintext.len() > 4096 {
            return Err(AssistantError::Credential);
        }
        Ok(Self {
            ciphertext: transform(plaintext, true, MESSAGING_IDENTITY_ENTROPY)?,
        })
    }
    pub fn from_ciphertext(ciphertext: Vec<u8>) -> Self {
        Self { ciphertext }
    }
    pub fn ciphertext(&self) -> &[u8] {
        &self.ciphertext
    }
    /// Short-lived plaintext; the buffer is overwritten when dropped.
    pub fn expose_identity_key(&self) -> Result<ExposedSecret, AssistantError> {
        if self.ciphertext.is_empty() || self.ciphertext.len() > 65_536 {
            return Err(AssistantError::Credential);
        }
        Ok(ExposedSecret(transform(
            &self.ciphertext,
            false,
            MESSAGING_IDENTITY_ENTROPY,
        )?))
    }
}
impl ExposedSecret {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[cfg(windows)]
fn transform(bytes: &[u8], protect: bool, entropy_bytes: &[u8]) -> Result<Vec<u8>, AssistantError> {
    use windows::Win32::{
        Foundation::{HLOCAL, LocalFree},
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: bytes.len() as u32,
        pbData: bytes.as_ptr().cast_mut(),
    };
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: entropy_bytes.len() as u32,
        pbData: entropy_bytes.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    // DPAPI copies input and allocates output; user-only scope, no UI or machine-wide flag.
    unsafe {
        let result = if protect {
            CryptProtectData(
                &input,
                None,
                Some(&entropy),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                None,
                Some(&entropy),
                None,
                None,
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        result.map_err(|_| AssistantError::Credential)?;
        if output.pbData.is_null() {
            return Err(AssistantError::Credential);
        }
        let slice = std::slice::from_raw_parts_mut(output.pbData, output.cbData as usize);
        let owned = slice.to_vec();
        for byte in slice {
            std::ptr::write_volatile(byte, 0);
        }
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        Ok(owned)
    }
}
#[cfg(not(windows))]
fn transform(_: &[u8], _: bool, _: &[u8]) -> Result<Vec<u8>, AssistantError> {
    Err(AssistantError::Credential)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reject_bad_secrets() {
        for value in ["", "key\r\ninjection", "a\0b"] {
            assert!(ProtectedSecret::protect(value).is_err());
        }
        assert!(ProtectedSecret { ciphertext: vec![] }.expose().is_err());
    }
    #[test]
    #[cfg(windows)]
    fn encrypted_roundtrip_and_debug_are_private() {
        let key = ProtectedSecret::protect("synthetic-test-secret").unwrap();
        assert_eq!(key.expose().unwrap().as_str(), "synthetic-test-secret");
        assert!(
            !serde_json::to_string(&key)
                .unwrap()
                .contains("synthetic-test-secret")
        );
        assert_eq!(format!("{key:?}"), "ProtectedSecret([redacted])");
        assert_eq!(
            format!("{:?}", key.expose().unwrap()),
            "ExposedSecret([redacted])"
        );
        let mut bad = key;
        bad.ciphertext[0] ^= 0xff;
        assert!(bad.expose().is_err());
    }
    #[test]
    #[cfg(windows)]
    fn identity_key_roundtrip_is_bound_to_its_purpose() {
        let seed = [7u8; 32];
        let key = ProtectedBytes::protect_identity_key(&seed).unwrap();
        assert_ne!(key.ciphertext(), &seed[..]);
        assert_eq!(key.expose_identity_key().unwrap().as_bytes(), &seed[..]);
        assert_eq!(format!("{key:?}"), "ProtectedBytes([redacted])");
        let as_assistant = ProtectedSecret {
            ciphertext: key.ciphertext().to_vec(),
        };
        assert!(as_assistant.expose().is_err());
        let restored = ProtectedBytes::from_ciphertext(key.ciphertext().to_vec());
        assert_eq!(
            restored.expose_identity_key().unwrap().as_bytes(),
            &seed[..]
        );
        assert!(ProtectedBytes::protect_identity_key(&[]).is_err());
    }
}
