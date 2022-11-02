#![cfg(feature = "full")]

pub use crate::message::{AddressLoader, SimpleAddressLoader};
use std::collections::HashSet;

use {
    super::SanitizedVersionedTransaction,
    crate::{
        hash::Hash,
        message::{
            v0::{self, LoadedAddresses},
            SanitizedMessage, VersionedMessage,
        },
        precompiles::verify_if_precompile,
        pubkey::Pubkey,
        sanitize::Sanitize,
        signature::Signature,
        solana_sdk::feature_set,
        transaction::{Result, Transaction, TransactionError, VersionedTransaction},
    },
    solana_program::message::SanitizedVersionedMessage,
    std::sync::Arc,
};

/// Maximum number of accounts that a transaction may lock.
/// 128 was chosen because it is the minimum number of accounts
/// needed for the Neon EVM implementation.
pub const MAX_TX_ACCOUNT_LOCKS: usize = 128;

#[derive(Debug, Clone)]
pub struct MevPoolKeys {
    pub pool: Pubkey,
    pub source: Option<Pubkey>,
    pub destination: Option<Pubkey>,
    pub token_a: Pubkey,
    pub token_b: Pubkey,
    pub pool_mint: Pubkey,
    pub pool_fee: Pubkey,
    pub pool_authority: Pubkey,
}

#[derive(Debug, Clone)]
pub struct MevKeys {
    pub pool_keys: Vec<MevPoolKeys>,
    pub token_program: Pubkey,
    pub user_authority: Option<Pubkey>,
}

impl MevKeys {
    pub fn get_readonly_accounts<'a>(&'a self, readonly_accounts: &mut HashSet<&'a Pubkey>) {
        for pool_keys in &self.pool_keys {
            readonly_accounts.insert(&pool_keys.pool);
            readonly_accounts.insert(&pool_keys.pool_authority);
            if pool_keys.source.is_some() && pool_keys.destination.is_some() {
                continue;
            }
            readonly_accounts.insert(&pool_keys.token_a);
            readonly_accounts.insert(&pool_keys.token_b);
            readonly_accounts.insert(&pool_keys.pool_mint);
            readonly_accounts.insert(&pool_keys.pool_fee);
        }
        if let Some(user_authority) = &self.user_authority {
            readonly_accounts.insert(user_authority);
        }
        readonly_accounts.insert(&self.token_program);
    }

    pub fn get_write_accounts<'a>(&'a self, write_accounts: &mut HashSet<&'a Pubkey>) {
        for pool_keys in &self.pool_keys {
            match (&pool_keys.source, &pool_keys.destination) {
                (Some(source), Some(destination)) => {
                    write_accounts.insert(source);
                    write_accounts.insert(destination);
                    write_accounts.insert(&pool_keys.token_a);
                    write_accounts.insert(&pool_keys.token_b);
                    write_accounts.insert(&pool_keys.pool_mint);
                    write_accounts.insert(&pool_keys.pool_fee);
                }
                _ => continue,
            }
        }
    }
}

/// Sanitized transaction and the hash of its message
#[derive(Debug, Clone)]
pub struct SanitizedTransaction {
    message: SanitizedMessage,
    message_hash: Hash,
    is_simple_vote_tx: bool,
    signatures: Vec<Signature>,
    // Store MEV monitored accounts to be loaded.
    pub mev_keys: Option<MevKeys>,
}

/// Set of accounts that must be locked for safe transaction processing
#[derive(Debug, Clone)]
pub struct TransactionAccountLocks<'a> {
    /// List of readonly account key locks
    pub readonly: Vec<&'a Pubkey>,
    /// List of writable account key locks
    pub writable: Vec<&'a Pubkey>,
    /// List of MEV readonly account key locks
    pub readonly_mev: Option<&'a MevKeys>,
}

/// Type that represents whether the transaction message has been precomputed or
/// not.
pub enum MessageHash {
    Precomputed(Hash),
    Compute,
}

impl From<Hash> for MessageHash {
    fn from(hash: Hash) -> Self {
        Self::Precomputed(hash)
    }
}

impl SanitizedTransaction {
    /// Create a sanitized transaction from a sanitized versioned transaction.
    /// If the input transaction uses address tables, attempt to lookup the
    /// address for each table index.
    pub fn try_new(
        tx: SanitizedVersionedTransaction,
        message_hash: Hash,
        is_simple_vote_tx: bool,
        address_loader: impl AddressLoader,
    ) -> Result<Self> {
        let signatures = tx.signatures;
        let SanitizedVersionedMessage { message } = tx.message;
        let message = match message {
            VersionedMessage::Legacy(message) => SanitizedMessage::Legacy(message),
            VersionedMessage::V0(message) => {
                let loaded_addresses =
                    address_loader.load_addresses(&message.address_table_lookups)?;
                SanitizedMessage::V0(v0::LoadedMessage::new(message, loaded_addresses))
            }
        };

        Ok(Self {
            message,
            message_hash,
            is_simple_vote_tx,
            signatures,
            mev_keys: None,
        })
    }

    /// Create a sanitized transaction from an un-sanitized versioned
    /// transaction.  If the input transaction uses address tables, attempt to
    /// lookup the address for each table index.
    pub fn try_create(
        tx: VersionedTransaction,
        message_hash: impl Into<MessageHash>,
        is_simple_vote_tx: Option<bool>,
        address_loader: impl AddressLoader,
        require_static_program_ids: bool,
    ) -> Result<Self> {
        tx.sanitize(require_static_program_ids)?;

        let message_hash = match message_hash.into() {
            MessageHash::Compute => tx.message.hash(),
            MessageHash::Precomputed(hash) => hash,
        };

        let signatures = tx.signatures;
        let message = match tx.message {
            VersionedMessage::Legacy(message) => SanitizedMessage::Legacy(message),
            VersionedMessage::V0(message) => {
                let loaded_addresses =
                    address_loader.load_addresses(&message.address_table_lookups)?;
                SanitizedMessage::V0(v0::LoadedMessage::new(message, loaded_addresses))
            }
        };

        let is_simple_vote_tx = is_simple_vote_tx.unwrap_or_else(|| {
            // TODO: Move to `vote_parser` runtime module
            let mut ix_iter = message.program_instructions_iter();
            ix_iter.next().map(|(program_id, _ix)| program_id) == Some(&crate::vote::program::id())
        });

        Ok(Self {
            message,
            message_hash,
            is_simple_vote_tx,
            signatures,
            mev_keys: None,
        })
    }

    pub fn try_from_legacy_transaction(tx: Transaction) -> Result<Self> {
        tx.sanitize()?;

        Ok(Self {
            message_hash: tx.message.hash(),
            message: SanitizedMessage::Legacy(tx.message),
            is_simple_vote_tx: false,
            signatures: tx.signatures,
            mev_keys: None,
        })
    }

    /// Create a sanitized transaction from a legacy transaction. Used for tests only.
    pub fn from_transaction_for_tests(tx: Transaction) -> Self {
        Self::try_from_legacy_transaction(tx).unwrap()
    }

    /// Return the first signature for this transaction.
    ///
    /// Notes:
    ///
    /// Sanitized transactions must have at least one signature because the
    /// number of signatures must be greater than or equal to the message header
    /// value `num_required_signatures` which must be greater than 0 itself.
    pub fn signature(&self) -> &Signature {
        &self.signatures[0]
    }

    /// Return the list of signatures for this transaction
    pub fn signatures(&self) -> &[Signature] {
        &self.signatures
    }

    /// Return the signed message
    pub fn message(&self) -> &SanitizedMessage {
        &self.message
    }

    /// Return the hash of the signed message
    pub fn message_hash(&self) -> &Hash {
        &self.message_hash
    }

    /// Returns true if this transaction is a simple vote
    pub fn is_simple_vote_transaction(&self) -> bool {
        self.is_simple_vote_tx
    }

    /// Convert this sanitized transaction into a versioned transaction for
    /// recording in the ledger.
    pub fn to_versioned_transaction(&self) -> VersionedTransaction {
        let signatures = self.signatures.clone();
        match &self.message {
            SanitizedMessage::V0(sanitized_msg) => VersionedTransaction {
                signatures,
                message: VersionedMessage::V0(v0::Message::clone(&sanitized_msg.message)),
            },
            SanitizedMessage::Legacy(message) => VersionedTransaction {
                signatures,
                message: VersionedMessage::Legacy(message.clone()),
            },
        }
    }

    /// Validate and return the account keys locked by this transaction
    pub fn get_account_locks(
        &self,
        tx_account_lock_limit: usize,
    ) -> Result<TransactionAccountLocks> {
        if self.message.has_duplicates() {
            Err(TransactionError::AccountLoadedTwice)
        } else if self.message.account_keys().len() > tx_account_lock_limit {
            Err(TransactionError::TooManyAccountLocks)
        } else {
            Ok(self.get_account_locks_unchecked())
        }
    }

    /// Return the list of accounts that must be locked during processing this transaction.
    pub fn get_account_locks_unchecked(&self) -> TransactionAccountLocks {
        let message = &self.message;
        let account_keys = message.account_keys();
        let num_readonly_accounts = message.num_readonly_accounts();
        let num_writable_accounts = account_keys.len().saturating_sub(num_readonly_accounts);

        let mut account_locks = TransactionAccountLocks {
            writable: Vec::with_capacity(num_writable_accounts),
            readonly: Vec::with_capacity(num_readonly_accounts),
            readonly_mev: self.mev_keys.as_ref(),
        };

        for (i, key) in account_keys.iter().enumerate() {
            if message.is_writable(i) {
                account_locks.writable.push(key);
            } else {
                account_locks.readonly.push(key);
            }
        }

        account_locks
    }

    /// Return the list of addresses loaded from on-chain address lookup tables
    pub fn get_loaded_addresses(&self) -> LoadedAddresses {
        match &self.message {
            SanitizedMessage::Legacy(_) => LoadedAddresses::default(),
            SanitizedMessage::V0(message) => LoadedAddresses::clone(&message.loaded_addresses),
        }
    }

    /// If the transaction uses a durable nonce, return the pubkey of the nonce account
    pub fn get_durable_nonce(&self, nonce_must_be_writable: bool) -> Option<&Pubkey> {
        self.message.get_durable_nonce(nonce_must_be_writable)
    }

    /// Return the serialized message data to sign.
    fn message_data(&self) -> Vec<u8> {
        match &self.message {
            SanitizedMessage::Legacy(message) => message.serialize(),
            SanitizedMessage::V0(loaded_msg) => loaded_msg.message.serialize(),
        }
    }

    /// Verify the transaction signatures
    pub fn verify(&self) -> Result<()> {
        let message_bytes = self.message_data();
        if self
            .signatures
            .iter()
            .zip(self.message.account_keys().iter())
            .map(|(signature, pubkey)| signature.verify(pubkey.as_ref(), &message_bytes))
            .any(|verified| !verified)
        {
            Err(TransactionError::SignatureFailure)
        } else {
            Ok(())
        }
    }

    /// Verify the precompiled programs in this transaction
    pub fn verify_precompiles(&self, feature_set: &Arc<feature_set::FeatureSet>) -> Result<()> {
        for (program_id, instruction) in self.message.program_instructions_iter() {
            verify_if_precompile(
                program_id,
                instruction,
                self.message().instructions(),
                feature_set,
            )
            .map_err(|_| TransactionError::InvalidAccountIndex)?;
        }
        Ok(())
    }
}
