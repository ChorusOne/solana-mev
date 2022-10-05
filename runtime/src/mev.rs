pub mod arbitrage;
pub mod utils;

use std::{collections::HashMap, fs, io::Write, sync::Arc, thread::JoinHandle};

use crossbeam_channel::{unbounded, Sender};
use log::error;
use serde::{
    ser::{SerializeMap, SerializeStruct},
    Serialize, Serializer,
};
use solana_sdk::{
    account::ReadableAccount, clock::Slot, hash::Hash, pubkey::Pubkey, signature::Signature,
    transaction::SanitizedTransaction,
};
use spl_token::solana_program::{program_error::ProgramError, program_pack::Pack};
use spl_token_swap::state::SwapVersion;

use crate::{
    accounts::LoadedTransaction,
    mev::utils::{deserialize_b58, serialize_b58},
};

use self::{
    arbitrage::{get_pre_defined_arbitrage_path, MevPath},
    utils::{AllOrcaPoolAddresses, MevConfig},
};

/// MevLog saves the `log_send_channel` channel, where it can be passed and
/// cloned in the `Bank` structure. We spawn a thread on the initialization of
/// the struct to listen and log data in `log_path`.
#[derive(Debug)]
pub struct MevLog {
    pub thread_handle: JoinHandle<()>,
    pub log_send_channel: Sender<MevMsg>,
}

#[derive(Debug, Clone)]
pub struct Mev {
    pub log_send_channel: Sender<MevMsg>,
    pub orca_program: Pubkey,

    // These public keys are going to be loaded so we can ensure no other thread
    // modifies the data we are interested in.
    // TODO: Change this to pairs we are willing to trade on.
    pub orca_monitored_accounts: Arc<AllOrcaPoolAddresses>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
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
    pool_a_balance: u64,
    pool_b_balance: u64,
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

// A map from `Pubkey` as `String` to `OrcaPoolWithBalance` so it's easier to
// serialize with `serde_json`
#[derive(Debug)]
pub struct PoolStates(HashMap<Pubkey, OrcaPoolWithBalance>);

impl Serialize for PoolStates {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            map.serialize_entry(&k.to_string(), &v)?;
        }
        map.end()
    }
}

pub enum MevMsg {
    Log(PrePostPoolStates),
    Opportunity(MevPath),
    Exit,
}

#[derive(Debug, Serialize)]
pub struct PrePostPoolStates {
    /// Transaction hash which triggered the MEV.
    #[serde(serialize_with = "serialize_b58")]
    transaction_hash: Hash,

    /// The first signature of the transaction.
    ///
    /// Block explorers identify transactions by the first signature, not by the
    /// transaction hash, so we also keep the signature for cross-referencing.
    #[serde(serialize_with = "serialize_b58")]
    transaction_signature: Signature,

    slot: Slot,

    orca_pre_tx_pool: PoolStates,
    orca_post_tx_pool: PoolStates,
}

impl Mev {
    pub fn new(log_send_channel: Sender<MevMsg>, config: MevConfig) -> Self {
        Mev {
            log_send_channel,
            orca_program: config.orca_program_id,
            orca_monitored_accounts: Arc::new(config.orca_accounts),
        }
    }

    /// Fill the field of `transaction.mev_accounts` with accounts we are
    /// interested in watching.
    pub fn fill_tx_mev_accounts(&self, tx: &mut SanitizedTransaction) {
        if self.is_monitored_account(tx) {
            for orca_pool in self.orca_monitored_accounts.0.iter() {
                tx.mev_keys.push([
                    orca_pool.address,
                    orca_pool.pool_a_account,
                    orca_pool.pool_b_account,
                ]);
            }
        }
    }

    /// Attempts to deserialize the Orca accounts MEV is interested in,
    /// in case the deserialization fails for some reason, returns the error.
    fn get_all_orca_monitored_accounts(
        &self,
        loaded_transaction: &LoadedTransaction,
    ) -> Result<PoolStates, ProgramError> {
        let pool_states = loaded_transaction
            .mev_accounts.iter()
            .map(|s| {
                let [
                    (pool_key, pool_account),
                    (pool_a_key, pool_a_account),
                    (pool_b_key, pool_b_account),
                    ] = [&s[0], &s[1], &s[2]];

                let pool = SwapVersion::unpack(pool_account.data())?;

                let pool_a_account = spl_token::state::Account::unpack(pool_a_account.data())?;
                let pool_b_account = spl_token::state::Account::unpack(pool_b_account.data())?;
                Ok((*pool_key, OrcaPoolWithBalance {
                    pool: OrcaPoolAddresses {
                        address: *pool_key,
                        pool_a_account: *pool_a_key,
                        pool_b_account: *pool_b_key,
                    },
                    pool_a_balance: pool_a_account.amount,
                    pool_b_balance: pool_b_account.amount,
                    fees: Fees(pool.fees().clone()),
                }))
            })
            .collect::<Result<HashMap<Pubkey,OrcaPoolWithBalance>, ProgramError>>()?;
        Ok(PoolStates(pool_states))
    }

    pub fn is_monitored_account(&self, tx: &SanitizedTransaction) -> bool {
        tx.message()
            .account_keys()
            .iter()
            .any(|account_key| &self.orca_program == account_key)
    }

    pub fn get_pre_tx_pool_state(
        &self,
        tx: &SanitizedTransaction,
        loaded_transaction: &mut LoadedTransaction,
    ) -> Option<PoolStates> {
        if !tx.mev_keys.is_empty() {
            self.get_all_orca_monitored_accounts(loaded_transaction)
                .ok()
        } else {
            None
        }
    }

    /// Execute and log the pool state after a transaction interacted with one or more
    /// account from the pool.
    pub fn execute_and_log_mev_opportunities(
        &self,
        tx: &SanitizedTransaction,
        loaded_transaction: &mut LoadedTransaction,
        slot: Slot,
        pre_tx_pool_state: PoolStates,
    ) -> Option<()> {
        let post_tx_pool_state = self
            .get_all_orca_monitored_accounts(loaded_transaction)
            .ok()?;
        let mev_path_opt = get_pre_defined_arbitrage_path(&post_tx_pool_state);

        if let Err(err) = self.log_send_channel.send(MevMsg::Log(PrePostPoolStates {
            transaction_hash: *tx.message_hash(),
            transaction_signature: *tx.signature(),
            slot,
            orca_pre_tx_pool: pre_tx_pool_state,
            orca_post_tx_pool: post_tx_pool_state,
        })) {
            error!("[MEV] Could not log pool states, error: {}", err);
        }
        if let Some(mev_path) = mev_path_opt {
            if let Err(err) = self.log_send_channel.send(MevMsg::Opportunity(mev_path)) {
                error!("[MEV] Could not log arbitrage, error: {}", err);
            }
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

        let thread_handle = std::thread::spawn(move || loop {
            match log_receiver.recv() {
                Ok(MevMsg::Log(msg)) => writeln!(
                    file,
                    "{}",
                    serde_json::to_string(&msg).expect("Constructed by us, should never fail")
                )
                .expect("[MEV] Could not write log to file"),

                Ok(MevMsg::Opportunity(mev_path)) => writeln!(
                    file,
                    "{}",
                    serde_json::to_string(&mev_path).expect("Constructed by us, should never fail")
                )
                .expect("[MEV] Could not write log opportunity to file"),

                Ok(MevMsg::Exit) => break,
                Err(err) => error!("[MEV] Could not log arbitrage on file, error: {}", err),
            }
        });

        MevLog {
            thread_handle,
            log_send_channel,
        }
    }
}

#[test]
fn test_log_serialization() {
    use std::str::FromStr;

    let opportunity = PrePostPoolStates {
        transaction_hash: Hash::new_unique(),
        transaction_signature: Signature::new(&[0; 64]),
        slot: 1,
        orca_pre_tx_pool: PoolStates(
            vec![(
                Pubkey::from_str("4uQeVj5tqViQh7yWWGStvkEG1Zmhx6uasJtWCJziofM").unwrap(),
                OrcaPoolWithBalance {
                    pool: OrcaPoolAddresses {
                        address: Pubkey::from_str("4uQeVj5tqViQh7yWWGStvkEG1Zmhx6uasJtWCJziofM")
                            .unwrap(),
                        pool_a_account: Pubkey::from_str(
                            "8opHzTAnfzRpPEx21XtnrVTX28YQuCpAjcn1PczScKh",
                        )
                        .unwrap(),
                        pool_b_account: Pubkey::from_str(
                            "CiDwVBFgWV9E5MvXWoLgnEgn2hK7rJikbvfWavzAQz3",
                        )
                        .unwrap(),
                    },
                    pool_a_balance: 1,
                    pool_b_balance: 1,
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
            )]
            .into_iter()
            .collect(),
        ),
        orca_post_tx_pool: PoolStates(HashMap::new()),
    };

    let expected_result_str = "\
    {\
        'transaction_hash':'4uQeVj5tqViQh7yWWGStvkEG1Zmhx6uasJtWCJziofM',\
        'transaction_signature':'1111111111111111111111111111111111111111111111111111111111111111',\
        'slot':1,\
        'orca_pre_tx_pool':{'4uQeVj5tqViQh7yWWGStvkEG1Zmhx6uasJtWCJziofM':\
          {\
            'pool':{\
              'address':'4uQeVj5tqViQh7yWWGStvkEG1Zmhx6uasJtWCJziofM',\
              'pool_a_account':'8opHzTAnfzRpPEx21XtnrVTX28YQuCpAjcn1PczScKh',\
              'pool_b_account':'CiDwVBFgWV9E5MvXWoLgnEgn2hK7rJikbvfWavzAQz3'\
            },\
            'pool_a_balance':1,\
            'pool_b_balance':1,\
            'fees':{\
              'host_fee_denominator':10,\
              'host_fee_numerator':1,\
              'owner_trade_fee_denominator':10,\
              'owner_trade_fee_numerator':1,\
              'trade_fee_denominator':10,\
              'trade_fee_numerator':1\
            }\
          }\
        },\
        'orca_post_tx_pool':{}\
      }"
    .replace("'", "\"");
    let serialized_json = serde_json::to_string(&opportunity).expect("Serialization failed");
    assert_eq!(serialized_json, expected_result_str);
}
