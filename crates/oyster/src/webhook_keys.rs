//! Helpers for generating, encoding, and using the per-app Ed25519 keypair
//! that signs outbound webhook deliveries.

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use ed25519_dalek::{Signature, Signer, SigningKey};
use rand::RngExt;

/// Length, in bytes, of an Ed25519 seed / public key.
pub const KEY_LEN: usize = 32;

/// Generate a fresh Ed25519 keypair from OS randomness.
///
/// Returns the `SigningKey` and the 32-byte public key. The signing key's
/// `to_bytes()` is the 32-byte seed and is stored as the per-app private key.
pub fn generate_keypair() -> (SigningKey, [u8; KEY_LEN]) {
    let mut seed = [0u8; KEY_LEN];
    rand::rng().fill(&mut seed);
    let signing_key = SigningKey::from_bytes(&seed);
    let public_key = signing_key.verifying_key().to_bytes();
    (signing_key, public_key)
}

/// Base64-encode (standard alphabet) the given bytes.
pub fn encode(bytes: &[u8]) -> String {
    BASE64.encode(bytes)
}

/// Decode a base64-encoded 32-byte key into a fixed-size array.
pub fn decode_key(encoded: &str) -> Result<[u8; KEY_LEN], String> {
    let bytes = BASE64
        .decode(encoded)
        .map_err(|e| format!("invalid base64: {e}"))?;
    bytes
        .try_into()
        .map_err(|v: Vec<u8>| format!("expected {KEY_LEN}-byte key, got {}", v.len()))
}

/// Sign `body` with the given signing key.
pub fn sign(signing_key: &SigningKey, body: &[u8]) -> Signature {
    signing_key.sign(body)
}

/// Build the `X-Oyster-Public-Key-Fingerprint` header value: hex of the
/// first 8 bytes of the public key.
pub fn fingerprint(public_key: &[u8; KEY_LEN]) -> String {
    hex::encode(&public_key[..8])
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Verifier, VerifyingKey};

    use super::*;

    #[test]
    fn generate_keypair_roundtrips() {
        let (signing_key, public_key) = generate_keypair();
        let body = b"hello world";
        let sig = sign(&signing_key, body);
        let vk = VerifyingKey::from_bytes(&public_key).unwrap();
        assert!(vk.verify(body, &sig).is_ok());
    }

    #[test]
    fn each_call_yields_distinct_key() {
        let (_, pk1) = generate_keypair();
        let (_, pk2) = generate_keypair();
        assert_ne!(pk1, pk2);
    }

    #[test]
    fn encode_decode_roundtrips() {
        let (signing_key, public_key) = generate_keypair();
        let priv_b64 = encode(signing_key.as_bytes());
        let pub_b64 = encode(&public_key);
        let priv_bytes = decode_key(&priv_b64).unwrap();
        let pub_bytes = decode_key(&pub_b64).unwrap();
        assert_eq!(priv_bytes, *signing_key.as_bytes());
        assert_eq!(pub_bytes, public_key);
    }

    #[test]
    fn decode_rejects_wrong_length() {
        let too_short = encode(&[0u8; 16]);
        assert!(decode_key(&too_short).is_err());
    }

    #[test]
    fn fingerprint_is_first_eight_bytes_hex() {
        let pk = [
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0xff, 0xfe, 0xfd, 0xfc, 0xfb, 0xfa,
            0xf9, 0xf8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ];
        assert_eq!(fingerprint(&pk), "0102030405060708");
    }
}
