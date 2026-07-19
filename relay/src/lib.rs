use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RegisterRequest {
    pub tenant_id: String,
    pub tenant_secret: String,
    pub registration_nonce: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PollResponse {
    pub request_id: String,
    pub path_and_query: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RelayResult {
    pub status: u16,
    pub content_type: String,
    pub body_base64: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ClientIdentity {
    pub tenant_id: String,
    pub tenant_secret: String,
    #[serde(default)]
    pub registration_nonce: u64,
}

impl ClientIdentity {
    pub fn generate() -> Self {
        let mut secret = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut secret);
        let tenant_secret = hex::encode(secret);
        let tenant_id = tenant_id_for_secret(&tenant_secret);
        Self {
            registration_nonce: solve_registration_proof(&tenant_id, &tenant_secret),
            tenant_id,
            tenant_secret,
        }
    }

    pub fn from_secret(tenant_secret: String) -> Self {
        let tenant_id = tenant_id_for_secret(&tenant_secret);
        Self {
            registration_nonce: solve_registration_proof(&tenant_id, &tenant_secret),
            tenant_id,
            tenant_secret,
        }
    }
}

pub fn valid_registration_proof(tenant_id: &str, secret: &str, nonce: u64) -> bool {
    let digest = Sha256::digest(format!("{tenant_id}:{secret}:{nonce}").as_bytes());
    digest[0] == 0 && digest[1] == 0 && digest[2] < 16
}

fn solve_registration_proof(tenant_id: &str, secret: &str) -> u64 {
    (0..u64::MAX)
        .find(|nonce| valid_registration_proof(tenant_id, secret, *nonce))
        .expect("registration proof search exhausted")
}

pub fn tenant_id_for_secret(secret: &str) -> String {
    hex::encode(&Sha256::digest(secret.as_bytes())[..16])
}

pub fn valid_tenant_id(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn valid_secret(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub fn secret_hash(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

pub fn secret_hash_matches(expected_hex: &str, secret: &str) -> bool {
    let Ok(expected) = hex::decode(expected_hex) else {
        return false;
    };
    let actual = Sha256::digest(secret.as_bytes());
    expected.len() == actual.len() && bool::from(expected.as_slice().ct_eq(actual.as_slice()))
}

pub fn valid_image_path(path: &str) -> bool {
    let Some(path) = path.strip_prefix("/image/") else {
        return false;
    };
    let filename = path.split('?').next().unwrap_or_default();
    let Some((digest, extension)) = filename.split_once('.') else {
        return false;
    };
    digest.len() == 64
        && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        && matches!(extension, "png" | "jpg" | "webp" | "gif")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_are_independent() {
        let first = ClientIdentity::generate();
        let second = ClientIdentity::generate();
        assert!(valid_tenant_id(&first.tenant_id));
        assert!(valid_secret(&first.tenant_secret));
        assert_ne!(first.tenant_id, second.tenant_id);
        assert_eq!(first.tenant_id, tenant_id_for_secret(&first.tenant_secret));
        assert!(valid_registration_proof(
            &first.tenant_id,
            &first.tenant_secret,
            first.registration_nonce
        ));
        assert_ne!(
            secret_hash(&first.tenant_secret),
            secret_hash(&second.tenant_secret)
        );
    }

    #[test]
    fn image_paths_are_narrow() {
        assert!(valid_image_path(&format!(
            "/image/{}.png?expires=1&sig=2",
            "a".repeat(64)
        )));
        assert!(!valid_image_path("/image/../../etc/passwd"));
        assert!(!valid_image_path(&format!("/image/{}.svg", "a".repeat(64))));
    }

    #[test]
    fn secret_hash_comparison_rejects_invalid_values() {
        let secret = "a".repeat(64);
        assert!(secret_hash_matches(&secret_hash(&secret), &secret));
        assert!(!secret_hash_matches(&secret_hash(&secret), &"b".repeat(64)));
        assert!(!secret_hash_matches("invalid", &secret));
    }
}
