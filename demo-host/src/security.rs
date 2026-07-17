use std::env;

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use rand::{RngCore, rngs::OsRng};
use sha2::{Digest, Sha256};

const LOGIN_TOKEN_BYTES: usize = 32;
const CREDENTIAL_NONCE_BYTES: usize = 12;

#[derive(Debug, thiserror::Error)]
pub enum SecurityError {
    #[error("DEMO_CREDENTIAL_ENCRYPTION_KEY is not set")]
    MissingCredentialEncryptionKey,
    #[error("DEMO_CREDENTIAL_ENCRYPTION_KEY must be base64-encoded 32-byte data")]
    InvalidCredentialEncryptionKey,
    #[error("credential encryption failed")]
    CredentialEncryption,
    #[error("credential decryption failed")]
    CredentialDecryption,
    #[error("decrypted credential is not valid UTF-8")]
    InvalidCredentialText,
}

#[derive(Clone)]
pub struct CredentialCipher {
    cipher: Aes256Gcm,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedCredential {
    pub ciphertext: Vec<u8>,
    pub nonce: [u8; CREDENTIAL_NONCE_BYTES],
}

impl CredentialCipher {
    pub fn from_env() -> Result<Self, SecurityError> {
        let encoded_key = env::var("DEMO_CREDENTIAL_ENCRYPTION_KEY")
            .map_err(|_| SecurityError::MissingCredentialEncryptionKey)?;
        let key = STANDARD
            .decode(encoded_key.trim())
            .map_err(|_| SecurityError::InvalidCredentialEncryptionKey)?;
        Self::from_key_bytes(&key)
    }

    pub fn from_key_bytes(key: &[u8]) -> Result<Self, SecurityError> {
        let cipher = Aes256Gcm::new_from_slice(key)
            .map_err(|_| SecurityError::InvalidCredentialEncryptionKey)?;
        Ok(Self { cipher })
    }

    pub fn encrypt(
        &self,
        user_id: &str,
        profile_id: &str,
        api_key: &str,
    ) -> Result<EncryptedCredential, SecurityError> {
        let mut nonce = [0_u8; CREDENTIAL_NONCE_BYTES];
        OsRng.fill_bytes(&mut nonce);
        let associated_data = credential_associated_data(user_id, profile_id);
        let ciphertext = self
            .cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: api_key.as_bytes(),
                    aad: associated_data.as_bytes(),
                },
            )
            .map_err(|_| SecurityError::CredentialEncryption)?;
        Ok(EncryptedCredential { ciphertext, nonce })
    }

    pub fn decrypt(
        &self,
        user_id: &str,
        profile_id: &str,
        encrypted: &EncryptedCredential,
    ) -> Result<String, SecurityError> {
        let associated_data = credential_associated_data(user_id, profile_id);
        let plaintext = self
            .cipher
            .decrypt(
                Nonce::from_slice(&encrypted.nonce),
                Payload {
                    msg: &encrypted.ciphertext,
                    aad: associated_data.as_bytes(),
                },
            )
            .map_err(|_| SecurityError::CredentialDecryption)?;
        String::from_utf8(plaintext).map_err(|_| SecurityError::InvalidCredentialText)
    }
}

pub fn hash_password(password: &str) -> String {
    password_auth::generate_hash(password)
}

pub fn password_matches(password: &str, password_hash: &str) -> bool {
    password_auth::verify_password(password, password_hash).is_ok()
}

pub fn generate_login_token() -> String {
    let mut token = [0_u8; LOGIN_TOKEN_BYTES];
    OsRng.fill_bytes(&mut token);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token)
}

pub fn hash_login_token(login_token: &str) -> [u8; 32] {
    Sha256::digest(login_token.as_bytes()).into()
}

fn credential_associated_data(user_id: &str, profile_id: &str) -> String {
    format!("traceable-model-profile:{user_id}:{profile_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cipher() -> CredentialCipher {
        CredentialCipher::from_key_bytes(&[7_u8; 32]).unwrap()
    }

    #[test]
    fn credential_round_trip_requires_matching_owner_and_profile() {
        let encrypted = cipher()
            .encrypt("user-a", "profile-a", "secret-key")
            .unwrap();
        assert_eq!(
            cipher().decrypt("user-a", "profile-a", &encrypted).unwrap(),
            "secret-key"
        );
        assert!(cipher().decrypt("user-b", "profile-a", &encrypted).is_err());
        assert!(cipher().decrypt("user-a", "profile-b", &encrypted).is_err());
    }

    #[test]
    fn login_tokens_are_random_and_only_hashes_need_persistence() {
        let first = generate_login_token();
        let second = generate_login_token();
        assert_ne!(first, second);
        assert_eq!(hash_login_token(&first).len(), 32);
        assert_ne!(hash_login_token(&first), hash_login_token(&second));
    }

    #[test]
    fn passwords_use_salted_hashes() {
        let first = hash_password("a sufficiently long password");
        let second = hash_password("a sufficiently long password");
        assert_ne!(first, second);
        assert!(password_matches("a sufficiently long password", &first));
        assert!(!password_matches("wrong password", &first));
    }
}
