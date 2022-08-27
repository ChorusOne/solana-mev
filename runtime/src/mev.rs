pub mod utils;

use std::{fs, io::Write, sync::Arc};

use crossbeam_channel::{unbounded, Sender};
use log::error;
use serde::{ser::SerializeStruct, Serialize};
use solana_sdk::{
    account::ReadableAccount, clock::Slot, hash::Hash, pubkey::Pubkey,
    transaction::SanitizedTransaction,
};
use spl_token::solana_program::{program_error::ProgramError, program_pack::Pack};
use spl_token_swap::state::SwapVersion;

use crate::{
    accounts::LoadedTransaction,
    mev::utils::{deserialize_b58, serialize_b58},
};

use self::utils::{AllOrcaPoolAddresses, MevConfig};

/// MevLog saves the `log_send_channel` channel, where it can be passed and
/// cloned in the `Bank` structure. We spawn a thread on the initialization of
/// the struct to listen and log data in `log_path`.
#[derive(Debug)]
pub struct MevLog {
    pub log_send_channel: Sender<PrePostPoolState>,
}

#[derive(Debug, Clone)]
pub struct Mev {
    pub log_send_channel: Sender<PrePostPoolState>,
    pub orca_program: Pubkey,

    // These public keys are going to be loaded so we can ensure no other thread
    // modifies the data we are interested in.
    // TODO: Change this to pairs we are willing to trade on.
    pub orca_interesting_accounts: Arc<AllOrcaPoolAddresses>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct OrcaPoolAddresses {
    #[serde(serialize_with = "serialize_b58")]
    #[serde(deserialize_with = "deserialize_b58")]
    address: Pubkey,

    /// Source address, owned by the pool.
    #[serde(serialize_with = "serialize_b58")]
    #[serde(deserialize_with = "deserialize_b58")]
    pool_a_account: Pubkey,

    /// Destination address, owned by the pool.
    #[serde(serialize_with = "serialize_b58")]
    #[serde(deserialize_with = "deserialize_b58")]
    pool_b_account: Pubkey,
}

#[derive(Debug, Serialize)]
pub struct OrcaPoolWithBalance {
    pool: OrcaPoolAddresses,
    pool_a_pre_balance: u64,
    pool_b_pre_balance: u64,
    fees: Fees,
}

#[derive(Debug)]
struct Fees(spl_token_swap::curve::fees::Fees);

impl Serialize for Fees {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("Fees", 3)?;
        state.serialize_field("host_fee_denominator", &self.0.host_fee_denominator)?;
        state.serialize_field("host_fee_numerator", &self.0.host_fee_numerator)?;
        state.serialize_field(
            "owner_trade_fee_denominator",
            &self.0.owner_trade_fee_denominator,
        )?;
        state.serialize_field(
            "owner_trade_fee_numerator",
            &self.0.owner_trade_fee_numerator,
        )?;
        state.serialize_field("trade_fee_denominator", &self.0.trade_fee_denominator)?;
        state.serialize_field("trade_fee_numerator", &self.0.trade_fee_numerator)?;
        state.end()
    }
}

type PoolState = Vec<OrcaPoolWithBalance>;

#[derive(Debug, Serialize)]
pub struct PrePostPoolState {
    /// Transaction hash which triggered the MEV.
    #[serde(serialize_with = "serialize_b58")]
    transaction_hash: Hash,
    slot: Slot,

    orca_pre_tx_pool: PoolState,
    orca_post_tx_pool: PoolState,
}

impl Mev {
    pub fn new(log_send_channel: Sender<PrePostPoolState>, config: MevConfig) -> Self {
        Mev {
            log_send_channel,
            orca_program: config.orca_program_id,
            orca_interesting_accounts: Arc::new(config.accounts),
        }
    }

    /// Fill the field of `transaction.mev_accounts` with accounts we are
    /// interested in watching.
    pub fn fill_tx_mev_accounts(&self, transaction: &mut SanitizedTransaction) {
        if transaction
            .message()
            .program_instructions_iter()
            .any(|(program_id, _compiled_ix)| &self.orca_program == program_id)
        {
            for orca_pool in self.orca_interesting_accounts.0.iter() {
                transaction.mev_keys.push([
                    orca_pool.address,
                    orca_pool.pool_a_account,
                    orca_pool.pool_b_account,
                ]);
            }
        }
    }

    /// Attempts to deserialize the Orca accounts MEV is interested in,
    /// in case the deserialization fails for some reason, returns the error.
    fn get_all_orca_interesting_accounts(
        &self,
        loaded_transaction: &LoadedTransaction,
    ) -> Result<Vec<OrcaPoolWithBalance>, ProgramError> {
        loaded_transaction
            .mev_accounts.iter()
            .map(|s| {
                // This should never overflow, we are the ones that construct
                // `loaded_transaction.mev_accounts` and we should ensure the
                // data is organized in triplets so we can later reconstruct it.
                let [
                    (pool_key, pool_account),
                    (pool_a_key, pool_a_account),
                    (pool_b_key, pool_b_account),
                    ] = [&s[0], &s[1], &s[2]];

                let pool = SwapVersion::unpack(pool_account.data())?;

                let pool_a_account = spl_token::state::Account::unpack(pool_a_account.data())?;
                let pool_b_account = spl_token::state::Account::unpack(pool_b_account.data())?;
                Ok(OrcaPoolWithBalance {
                    pool: OrcaPoolAddresses {
                        address: *pool_key,
                        pool_a_account: *pool_a_key,
                        pool_b_account: *pool_b_key,
                    },
                    pool_a_pre_balance: pool_a_account.amount,
                    pool_b_pre_balance: pool_b_account.amount,
                    fees: Fees(pool.fees().clone()),
                })
            })
            .collect()
    }

    pub fn get_pre_tx_pool_state(
        &self,
        tx: &SanitizedTransaction,
        loaded_transaction: &mut LoadedTransaction,
    ) -> Option<PoolState> {
        for addr in tx.message().account_keys().iter() {
            if addr == &self.orca_program {
                return self
                    .get_all_orca_interesting_accounts(loaded_transaction)
                    .ok();
            }
        }
        None
    }

    /// Execute and log the pool state after a transaction interacted with one or more
    /// account from the pool.
    pub fn execute_and_log_mev_opportunities(
        &self,
        tx: &SanitizedTransaction,
        loaded_transaction: &mut LoadedTransaction,
        slot: Slot,
        pre_tx_pool_state: PoolState,
    ) -> Option<()> {
        let post_tx_pool_state = self
            .get_all_orca_interesting_accounts(loaded_transaction)
            .ok()?;
        if let Err(err) = self.log_send_channel.send(PrePostPoolState {
            transaction_hash: *tx.message_hash(),
            slot,
            orca_pre_tx_pool: pre_tx_pool_state,
            orca_post_tx_pool: post_tx_pool_state,
        }) {
            error!("[MEV] Could not log arbitrage, error: {}", err);
        }

        // TODO: Return something once we exploit arbitrage opportunities.
        None
    }
}

impl MevLog {
    pub fn new(mev_config: &MevConfig) -> Self {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(true)
            .open(&mev_config.log_path)
            .expect("Failed while creating/opening MEV log file");
        let (log_send_channel, log_receiver) = unbounded();

        std::thread::spawn(move || loop {
            match log_receiver.recv() {
                Ok(msg) => writeln!(
                    file,
                    "{}",
                    serde_json::to_string(&msg).expect("Constructed by us, should never fail")
                )
                .expect("[MEV] Could not write to file"),
                Err(err) => error!("[MEV] Could not log arbitrage on file, error: {}", err),
            }
        });

        MevLog { log_send_channel }
    }
}

#[test]
fn test_serialization() {
    let opportunity = MevOpportunity {
        amount_in_a: 1,
        minimum_amount_out_b: 1,
        pool_with_balance: OrcaPoolWithBalance {
            pool: OrcaPoolAddresses {
                address: Pubkey::new_unique(),
                pool_a_account: Pubkey::new_unique(),
                pool_b_account: Pubkey::new_unique(),
            },
            pool_a_pre_balance: 1,
            pool_b_pre_balance: 1,
            fees: Fees(spl_token_swap::curve::fees::Fees {
                trade_fee_numerator: 1,
                trade_fee_denominator: 10,
                owner_trade_fee_numerator: 1,
                owner_trade_fee_denominator: 10,
                owner_withdraw_fee_numerator: 1,
                owner_withdraw_fee_denominator: 10,
                host_fee_numerator: 1,
                host_fee_denominator: 10,
            }),
        },
        user_a_account: Pubkey::new_unique(),
        user_a_pre_balance: 1,
        user_b_account: Pubkey::new_unique(),
        user_b_pre_balance: 1,
        a_token_mint: spl_token::solana_program::pubkey::Pubkey::new_from_array(
            Pubkey::new_unique().to_bytes(),
        ),
        b_token_mint: spl_token::solana_program::pubkey::Pubkey::new_from_array(
            Pubkey::new_unique().to_bytes(),
        ),

        transaction_hash: Hash::new_unique(),
        interesting_accounts: vec![],
        slot: 1,
    };

    let expected_result_str = "\
        {\
            'amount_in_a':1,\
            'minimum_amount_out_b':1,\
            'pool_with_balance':{\
            'pool':{\
                'address':'4uQeVj5tqViQh7yWWGStvkEG1Zmhx6uasJtWCJziofM',\
                'pool_a_account':'8opHzTAnfzRpPEx21XtnrVTX28YQuCpAjcn1PczScKh',\
                'pool_b_account':'CiDwVBFgWV9E5MvXWoLgnEgn2hK7rJikbvfWavzAQz3'\
            },\
            'pool_a_pre_balance':1,\
            'pool_b_pre_balance':1,\
            'fees':{\
                'host_fee_denominator':10,\
                'host_fee_numerator':1,\
                'owner_trade_fee_denominator':10,\
                'owner_trade_fee_numerator':1,\
                'trade_fee_denominator':10,\
                'trade_fee_numerator':1\
                }\
            },\
            'user_a_account':'GcdayuLaLyrdmUu324nahyv33G5poQdLUEZ1nEytDeP',\
            'user_a_pre_balance':1,\
            'user_b_account':'LX3EUdRUBUa3TbsYXLEUdj9J3prXkWXvLYSWyYyc2Jj',\
            'user_b_pre_balance':1,\
            'a_token_mint':'QRSsyMWN1yHT9ir42bgNZUNZ4PdEhcSWCrL2AryKpy5',\
            'b_token_mint':'UKrXU5bFrTzrqqpZXs8GVDbp4xPweiM65ADXNAy3ddR',\
            'transaction_hash':'4uQeVj5tqViQh7yWWGStvkEG1Zmhx6uasJtWCJziofM',\
            'interesting_accounts':[],\
            'slot':1\
        }"
    .replace("'", "\"");
    let serialized_json = serde_json::to_string(&opportunity).expect("Serialization failed");
    assert_eq!(serialized_json, expected_result_str);
}
