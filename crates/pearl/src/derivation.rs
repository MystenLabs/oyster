use fastcrypto::{ed25519::Ed25519KeyPair, traits::ToFromBytes};
use hkdf::Hkdf;
use sha2::Sha256;
use sui_types::{base_types::SuiAddress, crypto::SuiKeyPair};
use zeroize::Zeroizing;

const SALT: &[u8] = b"pearl-key-derivation-v1";

fn derive_keypair(master_seed: &[u8], account_id: &str) -> (SuiAddress, Zeroizing<Vec<u8>>) {
    let hk = Hkdf::<Sha256>::new(Some(SALT), master_seed);
    let mut okm = Zeroizing::new([0u8; 32]);
    hk.expand(account_id.as_bytes(), &mut *okm)
        .expect("32 bytes is a valid length for HKDF-SHA256");

    let kp = Ed25519KeyPair::from_bytes(&*okm).expect("32 bytes is a valid Ed25519 seed");
    let sui_kp = SuiKeyPair::Ed25519(kp);
    let address = SuiAddress::from(&sui_kp.public());
    let private_key_bytes = Zeroizing::new(okm.to_vec());
    (address, private_key_bytes)
}

/// Derive the Sui wallet address for an account (without exposing the private key).
pub fn derive_address(master_seed: &[u8], account_id: &str) -> String {
    let (address, _) = derive_keypair(master_seed, account_id);
    address.to_string()
}

/// Derive the Ed25519 private key bytes for an account.
pub fn derive_private_key(master_seed: &[u8], account_id: &str) -> Zeroizing<Vec<u8>> {
    let (_, private_key) = derive_keypair(master_seed, account_id);
    private_key
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_seed() -> Vec<u8> {
        vec![0xab; 32]
    }

    #[test]
    fn deterministic_same_inputs_same_output() {
        let seed = test_seed();
        let (addr1, key1) = derive_keypair(&seed, "account-1");
        let (addr2, key2) = derive_keypair(&seed, "account-1");
        assert_eq!(addr1, addr2);
        assert_eq!(key1, key2);
    }

    #[test]
    fn different_account_ids_different_keys() {
        let seed = test_seed();
        let (addr1, key1) = derive_keypair(&seed, "account-1");
        let (addr2, key2) = derive_keypair(&seed, "account-2");
        assert_ne!(addr1, addr2);
        assert_ne!(key1, key2);
    }

    #[test]
    fn different_seeds_different_keys() {
        let seed_a = vec![0xab; 32];
        let seed_b = vec![0xcd; 32];
        let (addr1, key1) = derive_keypair(&seed_a, "account-1");
        let (addr2, key2) = derive_keypair(&seed_b, "account-1");
        assert_ne!(addr1, addr2);
        assert_ne!(key1, key2);
    }

    #[test]
    fn derived_key_is_valid_ed25519() {
        let seed = test_seed();
        let (_, key) = derive_keypair(&seed, "test-account");
        assert_eq!(key.len(), 32);
        Ed25519KeyPair::from_bytes(&key).expect("valid Ed25519 key");
    }

    #[test]
    fn address_is_well_formed_sui_address() {
        let seed = test_seed();
        let address = derive_address(&seed, "test-account");
        assert!(address.starts_with("0x"));
        assert_eq!(address.len(), 66);
    }
}
