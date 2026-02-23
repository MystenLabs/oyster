use fastcrypto::{ed25519::Ed25519KeyPair, traits::ToFromBytes};
use shared_crypto::intent::{Intent, IntentMessage};
use sui_types::{
    crypto::{Signature, SuiKeyPair},
    transaction::{Transaction, TransactionData},
};

use crate::error::Error;

pub fn sign_transaction(private_key_bytes: &[u8], tx_data_bytes: &[u8]) -> Result<Vec<u8>, Error> {
    let ed25519_kp = Ed25519KeyPair::from_bytes(private_key_bytes)
        .map_err(|e| Error::InvalidPrivateKey(e.to_string()))?;
    let sui_kp = SuiKeyPair::Ed25519(ed25519_kp);

    let tx_data: TransactionData =
        bcs::from_bytes(tx_data_bytes).map_err(|e| Error::InvalidTransactionData(e.to_string()))?;

    let intent_msg = IntentMessage::new(Intent::sui_transaction(), &tx_data);
    let signature: Signature = Signature::new_secure(&intent_msg, &sui_kp);

    let transaction = Transaction::from_data(tx_data, vec![signature]);

    bcs::to_bytes(&transaction).map_err(|e| Error::SigningError(e.to_string()))
}

#[cfg(test)]
mod tests {
    use sui_types::{
        base_types::{ObjectDigest, ObjectID, SuiAddress},
        crypto::get_key_pair,
        programmable_transaction_builder::ProgrammableTransactionBuilder,
        transaction::TransactionData,
    };

    use super::*;

    fn mock_transaction_data(sender: SuiAddress) -> TransactionData {
        let gas_ref = (
            ObjectID::random(),
            sui_types::base_types::SequenceNumber::new(),
            ObjectDigest::random(),
        );
        let pt = ProgrammableTransactionBuilder::new().finish();
        TransactionData::new_programmable(sender, vec![gas_ref], pt, 5_000_000, 1_000)
    }

    #[test]
    fn sign_valid_transaction_data() {
        let (address, kp): (SuiAddress, Ed25519KeyPair) = get_key_pair();
        let private_key_bytes = kp.as_bytes().to_vec();

        let tx_data = mock_transaction_data(address);
        let tx_data_bytes = bcs::to_bytes(&tx_data).unwrap();

        let signed = sign_transaction(&private_key_bytes, &tx_data_bytes).unwrap();
        assert!(!signed.is_empty());

        // Verify we can deserialize the result back into a Transaction.
        let _tx: Transaction = bcs::from_bytes(&signed).unwrap();
    }

    #[test]
    fn sign_invalid_private_key() {
        let short_bytes = vec![0u8; 8];
        let tx_data_bytes = vec![1, 2, 3]; // doesn't matter — key check comes first

        let err = sign_transaction(&short_bytes, &tx_data_bytes).unwrap_err();
        assert!(
            matches!(err, Error::InvalidPrivateKey(_)),
            "expected InvalidPrivateKey, got: {err:?}"
        );
    }

    #[test]
    fn sign_invalid_tx_data() {
        let (_, kp): (SuiAddress, Ed25519KeyPair) = get_key_pair();
        let private_key_bytes = kp.as_bytes().to_vec();

        let garbage = vec![0xFF, 0xFE, 0xFD];

        let err = sign_transaction(&private_key_bytes, &garbage).unwrap_err();
        assert!(
            matches!(err, Error::InvalidTransactionData(_)),
            "expected InvalidTransactionData, got: {err:?}"
        );
    }
}
