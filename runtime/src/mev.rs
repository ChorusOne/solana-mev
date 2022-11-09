pub mod arbitrage;
pub mod utils;

use std::{
    collections::HashMap,
    fs::{self, File},
    io::{BufReader, Write},
    sync::Arc,
    thread::JoinHandle,
};

use crossbeam_channel::{unbounded, Sender};
use log::error;
use serde::{
    ser::{SerializeMap, SerializeStruct},
    Serialize, Serializer,
};
use solana_sdk::{
    account::ReadableAccount,
    clock::Slot,
    hash::Hash,
    pubkey::Pubkey,
    signature::{Keypair, Signature},
    signer::Signer,
    transaction::{MevKeys, MevPoolKeys, SanitizedTransaction},
};
use spl_token::solana_program::{program_error::ProgramError, program_pack::Pack};
use spl_token_swap::{curve::calculator::CurveCalculator, state::SwapVersion};

use crate::{
    accounts::LoadedTransaction,
    accounts::MevAccountOrIdx::{Idx, ReadAccount, WriteAccount},
    inline_spl_token,
    mev::utils::{deserialize_b58, serialize_b58},
};

use self::{
    arbitrage::{get_arbitrage_tx_outputs, MevOpportunityWithInput, MevPath, MevTxOutput},
    utils::{deserialize_opt_b58, serialize_opt_b58, AllOrcaPoolAddresses, MevConfig},
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

    // MEV paths that we are interested on finding an opportunity
    pub mev_paths: Vec<MevPath>,

    // Key for the user authority for signing transactions.
    // If `None`, we do not try to craft MEV txs.
    pub user_authority: Arc<Option<Keypair>>,
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

    /// Source address, owned by us.
    #[serde(default)]
    #[serde(serialize_with = "serialize_opt_b58")]
    #[serde(deserialize_with = "deserialize_opt_b58")]
    pub source: Option<Pubkey>,

    /// Destination address, owned by us.
    #[serde(default)]
    #[serde(serialize_with = "serialize_opt_b58")]
    #[serde(deserialize_with = "deserialize_opt_b58")]
    pub destination: Option<Pubkey>,

    /// Pool's mint account.
    #[serde(serialize_with = "serialize_b58")]
    #[serde(deserialize_with = "deserialize_b58")]
    pub pool_mint: Pubkey,

    /// Pool's fee account.
    #[serde(serialize_with = "serialize_b58")]
    #[serde(deserialize_with = "deserialize_b58")]
    pub pool_fee: Pubkey,

    /// Calculated by us from the pool's data.
    #[serde(skip_serializing)]
    #[serde(skip_deserializing)]
    pub pool_authority: Pubkey,
}

impl OrcaPoolAddresses {
    pub fn populate_pool_authority(&mut self) {
        let (pool_authority, _authority_bump_seed) =
            Pubkey::find_program_address(&[&self.address.to_bytes()[..]], &inline_spl_token::id());
        self.pool_authority = pool_authority;
    }
}

#[derive(Debug, Serialize)]
pub struct OrcaPoolWithBalance {
    pool: OrcaPoolAddresses,
    pool_a_balance: u64,
    pool_b_balance: u64,
    fees: Fees,

    #[serde(skip_serializing)]
    curve_calculator: Arc<dyn CurveCalculator + Sync + Send>,
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

impl FromIterator<(Pubkey, OrcaPoolWithBalance)> for PoolStates {
    fn from_iter<T: IntoIterator<Item = (Pubkey, OrcaPoolWithBalance)>>(iter: T) -> Self {
        let hashmap = HashMap::from_iter(iter);
        PoolStates(hashmap)
    }
}

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
    Opportunities(Vec<MevTxOutput>),
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
            mev_paths: config.mev_paths,
            user_authority: Arc::new(config.user_authority_path.map(|path| {
                let file = File::open(path).expect("[MEV] Could not open path");
                let reader = BufReader::new(file);
                let secret_key_bytes: Vec<u8> =
                    serde_json::from_reader(reader).expect("[MEV] Could not read authority path");
                Keypair::from_bytes(&secret_key_bytes)
                    .expect("[MEV] Could not generate Keypair from path")
            })),
        }
    }

    /// Fill the field of `transaction.mev_accounts` with accounts we are
    /// interested in watching.
    pub fn fill_tx_mev_accounts(&self, tx: &mut SanitizedTransaction) {
        if self.is_monitored_account(tx) {
            let pool_keys = self
                .orca_monitored_accounts
                .0
                .iter()
                .map(|orca_pool| MevPoolKeys {
                    pool: orca_pool.address,
                    source: orca_pool.source,
                    destination: orca_pool.destination,
                    token_a: orca_pool.pool_a_account,
                    token_b: orca_pool.pool_b_account,
                    pool_mint: orca_pool.pool_mint,
                    pool_fee: orca_pool.pool_fee,
                    pool_authority: orca_pool.pool_authority,
                })
                .collect();
            tx.mev_keys = Some(MevKeys {
                pool_keys,
                // Use SPL token ID for all pools.
                token_program: inline_spl_token::id(),
                user_authority: (*self.user_authority).as_ref().map(|kp| kp.pubkey()),
            })
        }
    }

    /// Attempts to deserialize the Orca accounts MEV is interested in,
    /// in case the deserialization fails for some reason, returns the error.
    pub fn get_all_orca_monitored_accounts<'a>(
        &self,
        loaded_transaction: &'a LoadedTransaction,
    ) -> Option<Result<PoolStates, ProgramError>> {
        let pool_states = loaded_transaction
            .mev_accounts
            .as_ref()
            .map(|mev_accounts| {
                mev_accounts
                    .pool_accounts
                    .iter()
                    .map(|mev_account| {
                        let get_account =
                            |pubkey: &'a Pubkey| match &mev_accounts.pubkey_account_map[pubkey] {
                                Idx(idx) => &loaded_transaction.accounts[*idx],
                                ReadAccount(acc) | WriteAccount(acc) => &acc,
                            };
                        let pool_acc = get_account(&mev_account.pool);
                        let pool = SwapVersion::unpack(pool_acc.1.data())?;

                        let pool_a_acc = get_account(&mev_account.token_a);
                        let pool_a_account =
                            spl_token::state::Account::unpack(pool_a_acc.1.data())?;

                        let pool_b_acc = get_account(&mev_account.token_b);
                        let pool_b_account =
                            spl_token::state::Account::unpack(pool_b_acc.1.data())?;

                        let pool_source_pubkey =
                            mev_account.source.as_ref().map(|src| get_account(src).0);

                        let pool_destination_pubkey = mev_account
                            .destination
                            .as_ref()
                            .map(|dst| get_account(dst).0);

                        let pool_mint_pubkey = get_account(&mev_account.pool_mint).0;
                        let pool_fee_pubkey = get_account(&mev_account.pool_fee).0;
                        let pool_authority_pubkey = get_account(&mev_account.pool_authority).0;

                        Ok((
                            pool_acc.0,
                            OrcaPoolWithBalance {
                                pool: OrcaPoolAddresses {
                                    address: pool_acc.0,
                                    pool_a_account: pool_a_acc.0,
                                    pool_b_account: pool_b_acc.0,
                                    source: pool_source_pubkey,
                                    destination: pool_destination_pubkey,
                                    pool_mint: pool_mint_pubkey,
                                    pool_fee: pool_fee_pubkey,
                                    pool_authority: pool_authority_pubkey,
                                },
                                pool_a_balance: pool_a_account.amount,
                                pool_b_balance: pool_b_account.amount,
                                fees: Fees(pool.fees().clone()),
                                curve_calculator: pool.swap_curve().calculator.clone(),
                            },
                        ))
                    })
                    .collect::<Result<PoolStates, ProgramError>>()
            });
        pool_states
    }

    pub fn is_monitored_account(&self, tx: &SanitizedTransaction) -> bool {
        tx.message()
            .account_keys()
            .iter()
            .any(|account_key| &self.orca_program == account_key)
    }

    /// Log the pool state after a transaction interacted with one or more
    /// account from the pool
    /// Returns the most profitable MEV tx.
    pub fn log_mev_opportunities_get_max_profit_tx(
        &self,
        tx: &SanitizedTransaction,
        slot: Slot,
        pre_tx_pool_state: PoolStates,
        loaded_tx: &LoadedTransaction,
        blockhash: Hash,
    ) -> Option<(Vec<SanitizedTransaction>, u64)> {
        let post_tx_pool_state = self.get_all_orca_monitored_accounts(loaded_tx)?.ok()?;
        let mut mev_tx_outputs = get_arbitrage_tx_outputs(
            &self.mev_paths,
            &post_tx_pool_state,
            self.orca_program,
            self.user_authority.as_ref().as_ref(),
            blockhash,
        );

        if let Err(err) = self.log_send_channel.send(MevMsg::Log(PrePostPoolStates {
            transaction_hash: *tx.message_hash(),
            transaction_signature: *tx.signature(),
            slot,
            orca_pre_tx_pool: pre_tx_pool_state,
            orca_post_tx_pool: post_tx_pool_state,
        })) {
            error!("[MEV] Could not log pool states, error: {}", err);
        }

        let mev_tx_output = mev_tx_outputs
            .iter_mut()
            .max_by(|a, b| a.profit.cmp(&b.profit))?;

        let mut sanitized_tx_vec = Vec::new();
        std::mem::swap(&mut sanitized_tx_vec, &mut mev_tx_output.sanitized_txs);
        let profit = mev_tx_output.profit;

        if let Err(err) = self
            .log_send_channel
            .send(MevMsg::Opportunities(mev_tx_outputs))
        {
            error!("[MEV] Could not log arbitrage, error: {}", err);
        }
        Some((sanitized_tx_vec, profit))
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

        let mev_paths = mev_config.mev_paths.clone();
        let thread_handle = std::thread::spawn(move || loop {
            match log_receiver.recv() {
                Ok(MevMsg::Log(msg)) => writeln!(
                    file,
                    "{}",
                    serde_json::to_string(&msg).expect("Constructed by us, should never fail")
                )
                .expect("[MEV] Could not write log to file"),

                Ok(MevMsg::Opportunities(mev_tx_output)) => {
                    let mev_paths_input: Vec<MevOpportunityWithInput> = mev_tx_output
                        .into_iter()
                        .map(|mev_tx_output| MevOpportunityWithInput {
                            opportunity: &mev_paths[mev_tx_output.path_idx],
                            input_output_pairs: mev_tx_output.input_output_pairs,
                        })
                        .collect();
                    writeln!(
                        file,
                        "{{\"event\":\"opportunity\",\"data\":{}}}",
                        serde_json::to_string(&mev_paths_input)
                            .expect("Constructed by us, should never fail")
                    )
                    .expect("[MEV] Could not write log opportunity to file")
                }

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
    use spl_token_swap::curve::constant_product::ConstantProductCurve;
    use std::str::FromStr;

    let curve_calculator = Arc::new(ConstantProductCurve::default());
    let (authority_pubkey, _authority_bump_seed) = Pubkey::find_program_address(
        &[
            &Pubkey::from_str("4uQeVj5tqViQh7yWWGStvkEG1Zmhx6uasJtWCJziofM")
                .unwrap()
                .to_bytes()[..],
        ],
        &inline_spl_token::id(),
    );

    let opportunity = PrePostPoolStates {
        transaction_hash: Hash::new(&[0; 32]),
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
                        source: None,
                        destination: None,
                        pool_mint: Pubkey::from_str("33k9G5HeH5JFukXTVxx3EmZrqjhb19Ej2GC2kqVPCKnM")
                            .unwrap(),
                        pool_fee: Pubkey::from_str("GqtosegQU4ad7W9AMHAQuuAFnjBQZ4VB4eZuPFrz8ALr")
                            .unwrap(),
                        pool_authority: authority_pubkey,
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
                    curve_calculator,
                },
            )]
            .into_iter()
            .collect(),
        ),
        orca_post_tx_pool: PoolStates(HashMap::new()),
    };

    let expected_result_str = "\
    {\
        'transaction_hash':'11111111111111111111111111111111',\
        'transaction_signature':'1111111111111111111111111111111111111111111111111111111111111111',\
        'slot':1,\
        'orca_pre_tx_pool':{'4uQeVj5tqViQh7yWWGStvkEG1Zmhx6uasJtWCJziofM':\
          {\
            'pool':{\
              'address':'4uQeVj5tqViQh7yWWGStvkEG1Zmhx6uasJtWCJziofM',\
              'pool_a_account':'8opHzTAnfzRpPEx21XtnrVTX28YQuCpAjcn1PczScKh',\
              'pool_b_account':'CiDwVBFgWV9E5MvXWoLgnEgn2hK7rJikbvfWavzAQz3',\
              'source':null,\
              'destination':null,\
              'pool_mint':'33k9G5HeH5JFukXTVxx3EmZrqjhb19Ej2GC2kqVPCKnM',\
              'pool_fee':'GqtosegQU4ad7W9AMHAQuuAFnjBQZ4VB4eZuPFrz8ALr'\
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
