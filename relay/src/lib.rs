use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RegisterRequest {
    pub tenant_id: String,
    pub tenant_secret: String,
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
}

impl ClientIdentity {
    pub fn generate() -> Self {
        let mut tenant = [0u8; 16];
        let mut secret = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut tenant);
        rand::rngs::OsRng.fill_bytes(&mut secret);
        Self {
            tenant_id: hex::encode(tenant),
            tenant_secret: hex::encode(secret),
        }
    }
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
}
