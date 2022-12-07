pub mod arbitrage;
pub mod utils;

use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{BufReader, Write},
    sync::Arc,
    thread::JoinHandle,
};

use crossbeam_channel::{unbounded, Sender};
use log::{error, warn};
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
use spl_token_swap::{
    curve::calculator::{CurveCalculator, SwapWithoutFeesResult},
    state::SwapVersion,
};

use crate::{
    accounts::LoadedTransaction,
    accounts::MevAccountOrIdx::{Idx, ReadAccount},
    inline_spl_token,
    mev::utils::{deserialize_b58, serialize_b58},
};

use self::{
    arbitrage::{
        create_swap_tx, InputOutputPairs, MevOpportunityWithInput, MevPath, MevTxOutput,
        SwapArguments, TradeDirection,
    },
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
    // A set of `Pubkey` for us to trigger MEV.
    pub watched_programs: HashSet<Pubkey>,

    // These public keys are going to be loaded so we can ensure no other thread
    // modifies the data we are interested in.
    // TODO: Change this to pairs we are willing to trade on.
    pub orca_monitored_accounts: Arc<AllOrcaPoolAddresses>,

    // MEV paths that we are interested on finding an opportunity
    pub mev_paths: Vec<MevPath>,

    // Key for the user authority for signing transactions.
    // If `None`, we do not try to craft MEV txs.
    pub user_authority: Arc<Option<Keypair>>,

    // A mapping with the minimum profit to execute MEV transactions token per
    // token address.
    pub minimum_profit: HashMap<Pubkey, u64>,
}

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct OrcaPoolAddresses {
    #[serde(skip_serializing)]
    #[serde(skip_deserializing)]
    program_id: Pubkey,

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

    /// Mint of pool's a account.
    #[serde(skip_serializing)]
    #[serde(skip_deserializing)]
    pub pool_a_mint: Pubkey,

    /// Mint of pool's b account.
    #[serde(skip_serializing)]
    #[serde(skip_deserializing)]
    pub pool_b_mint: Pubkey,
}

#[derive(Debug, Serialize)]
pub struct OrcaPoolWithBalance {
    pool: OrcaPoolAddresses,
    pool_a_balance: u64,
    pool_b_balance: u64,
    source_balance: Option<u64>,
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
    ExecutedTransaction(ExecutedTransactionOutput),
    Exit,
}

#[derive(Debug, Serialize)]
pub struct ExecutedTransactionOutput {
    #[serde(serialize_with = "serialize_b58")]
    pub transaction_hash: Hash,
    #[serde(serialize_with = "serialize_b58")]
    pub transaction_signature: Signature,

    pub is_successful: bool,
    pub possible_profit: u64,
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
        let mev_paths = config
            .mev_paths
            .into_iter()
            .map(|path| match (path.path.first(), path.path.last()) {
                (None, _) | (_, None) => panic!("MEV paths should have at least 1 element"),
                (Some(pair_a), Some(pair_b)) => {
                    if pair_a == pair_b {
                        panic!("MEV paths should not end in the same pool with the same direction of trade")
                    }
                    if pair_a.pool != pair_b.pool {
                        panic!(
                            "MEV paths should start and end at the same pool, \
path that starts with address {} finishes at address \
{}",
                            pair_a.pool, pair_b.pool
                        );
                    }
                    path
                }
            })
            .collect();
        Mev {
            log_send_channel,
            watched_programs: config
                .watched_programs
                .iter()
                .map(|b58pubkey| b58pubkey.0)
                .collect(),
            orca_monitored_accounts: Arc::new(config.orca_accounts),
            mev_paths,
            user_authority: Arc::new(config.user_authority_path.map(|path| {
                let file = File::open(path).expect("[MEV] Could not open path");
                let reader = BufReader::new(file);
                let secret_key_bytes: Vec<u8> =
                    serde_json::from_reader(reader).expect("[MEV] Could not read authority path");
                Keypair::from_bytes(&secret_key_bytes)
                    .expect("[MEV] Could not generate Keypair from path")
            })),
            minimum_profit: config
                .minimum_profit
                .into_iter()
                .map(|(b58_pubkey, min)| (b58_pubkey.0, min))
                .collect(),
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
    pub fn get_all_orca_monitored_accounts(
        &self,
        loaded_transaction: &LoadedTransaction,
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
                            |pubkey: &Pubkey| match &mev_accounts.pubkey_account_map[pubkey] {
                                Idx(idx) => &loaded_transaction.accounts[*idx],
                                ReadAccount(acc) => &acc,
                            };
                        let pool_acc = get_account(&mev_account.pool);
                        // Owner of the pool should be the `program_id`.
                        let program_id = pool_acc.1.owner();

                        let (pool_authority, _authority_bump_seed) = Pubkey::find_program_address(
                            &[&mev_account.pool.to_bytes()[..]],
                            &program_id,
                        );
                        let pool = SwapVersion::unpack(pool_acc.1.data())?;

                        let pool_a_acc = get_account(&mev_account.token_a);
                        let pool_a_account =
                            spl_token::state::Account::unpack(pool_a_acc.1.data())?;

                        let pool_b_acc = get_account(&mev_account.token_b);
                        let pool_b_account =
                            spl_token::state::Account::unpack(pool_b_acc.1.data())?;

                        let pool_source_pubkey_amount = mev_account
                            .source
                            .as_ref()
                            .map(|src| {
                                let (source_pubkey, source_account) = get_account(src);
                                let spl_acc =
                                    spl_token::state::Account::unpack(source_account.data())?;
                                Ok::<(&solana_sdk::pubkey::Pubkey, u64), ProgramError>((
                                    source_pubkey,
                                    spl_acc.amount,
                                ))
                            })
                            .transpose()?;

                        let pool_destination_pubkey = mev_account
                            .destination
                            .as_ref()
                            .map(|dst| get_account(dst).0);

                        let pool_mint_pubkey = get_account(&mev_account.pool_mint).0;
                        let pool_fee_pubkey = get_account(&mev_account.pool_fee).0;

                        Ok((
                            pool_acc.0,
                            OrcaPoolWithBalance {
                                pool: OrcaPoolAddresses {
                                    program_id: *program_id,
                                    address: pool_acc.0,
                                    pool_a_account: pool_a_acc.0,
                                    pool_b_account: pool_b_acc.0,
                                    source: pool_source_pubkey_amount.map(|(src, _amount)| *src),
                                    destination: pool_destination_pubkey,
                                    pool_mint: pool_mint_pubkey,
                                    pool_fee: pool_fee_pubkey,
                                    pool_authority: pool_authority,
                                    pool_a_mint: Pubkey::new(&pool_a_account.mint.to_bytes()),
                                    pool_b_mint: Pubkey::new(&pool_b_account.mint.to_bytes()),
                                },
                                pool_a_balance: pool_a_account.amount,
                                pool_b_balance: pool_b_account.amount,
                                fees: Fees(pool.fees().clone()),
                                curve_calculator: pool.swap_curve().calculator.clone(),
                                source_balance: pool_source_pubkey_amount
                                    .map(|(_src, amount)| amount),
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
            .any(|account_key| self.watched_programs.contains(account_key))
    }

    /// Log the pool state after a transaction interacted with one or more
    /// account from the pool
    /// Returns a tuple with the most profitable MEV tx and the profit in the
    /// token's unit.
    pub fn log_mev_opportunities_get_max_profit_tx(
        &self,
        tx: &SanitizedTransaction,
        slot: Slot,
        pre_tx_pool_state: PoolStates,
        loaded_tx: &LoadedTransaction,
        blockhash: Hash,
    ) -> Option<(SanitizedTransaction, u64)> {
        let post_tx_pool_state = self.get_all_orca_monitored_accounts(loaded_tx)?.ok()?;
        let mut mev_tx_outputs = self.get_arbitrage_tx_outputs(&post_tx_pool_state, blockhash);

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

        let profit = mev_tx_output.profit;
        let sanitized_tx = mev_tx_output.sanitized_tx.take();

        if let Err(err) = self
            .log_send_channel
            .send(MevMsg::Opportunities(mev_tx_outputs))
        {
            error!("[MEV] Could not log arbitrage, error: {}", err);
        }
        Some((sanitized_tx?, profit))
    }

    pub fn get_arbitrage_tx_outputs(
        &self,
        pool_states: &PoolStates,
        blockhash: Hash,
    ) -> Vec<MevTxOutput> {
        self.mev_paths
            .iter()
            .enumerate()
            .filter_map(|(path_idx, mev_path)| {
                let path_output = mev_path.get_path_calculation_output(pool_states)?;
                let initial_amount = path_output.optimal_input.floor() as u128;

                let initial_amount = if let Some(source_token_balance) = path_output.source_token_balance {
                    initial_amount.min(source_token_balance as u128)
                } else {
                    initial_amount
                };
                let mut amount_in = initial_amount;
                let mut input_output_pairs = Vec::with_capacity(mev_path.path.len());

                let mut swap_arguments_vec = Vec::with_capacity(mev_path.path.len());
                for pair_info in &mev_path.path {
                    let pool_state = pool_states.0.get(&pair_info.pool)?;

                    let trade_fee = pool_state.fees.0.trading_fee(amount_in)?;
                    let owner_fee = pool_state.fees.0.owner_trading_fee(amount_in)?;

                    let total_fees = trade_fee.checked_add(owner_fee)?;
                    let source_amount_less_fees = amount_in.checked_sub(total_fees)?;

                    let (
                        trade_direction,
                        source_pubkey,
                        swap_source_pubkey,
                        destination_pubkey,
                        swap_destination_pubkey,
                        swap_source_amount,
                        swap_destination_amount,
                    ) = match pair_info.direction {
                        TradeDirection::AtoB => (
                            spl_token_swap::curve::calculator::TradeDirection::AtoB,
                            pool_state.pool.source,
                            pool_state.pool.pool_a_account,
                            pool_state.pool.destination,
                            pool_state.pool.pool_b_account,
                            pool_state.pool_a_balance,
                            pool_state.pool_b_balance,
                        ),
                        TradeDirection::BtoA => (
                            spl_token_swap::curve::calculator::TradeDirection::BtoA,
                            pool_state.pool.destination,
                            pool_state.pool.pool_b_account,
                            pool_state.pool.source,
                            pool_state.pool.pool_a_account,
                            pool_state.pool_b_balance,
                            pool_state.pool_a_balance,
                        ),
                    };

                    // For the Constant Product Curve the `trade_direction` is
                    // ignored and it's our responsibility to provide the right
                    // token's balance from the pool.
                    let SwapWithoutFeesResult {
                        source_amount_swapped: _,
                        destination_amount_swapped,
                    } = pool_state.curve_calculator.swap_without_fees(
                        source_amount_less_fees,
                        swap_source_amount as u128,
                        swap_destination_amount as u128,
                        // Again, this argument is useless!
                        trade_direction,
                    )?;

                    input_output_pairs.push(InputOutputPairs {
                        token_in: amount_in as u64,
                        token_out: destination_amount_swapped as u64,
                    });

                    let swap_arguments = match (source_pubkey, destination_pubkey) {
                        (Some(source), Some(destination)) => Some(SwapArguments {
                            program_id: pool_state.pool.program_id,
                            swap_pubkey: pair_info.pool,
                            authority_pubkey: pool_state.pool.pool_authority,
                            source_pubkey: source,
                            swap_source_pubkey,
                            swap_destination_pubkey,
                            destination_pubkey: destination,
                            pool_mint_pubkey: pool_state.pool.pool_mint,
                            pool_fee_pubkey: pool_state.pool.pool_fee,
                            token_program: inline_spl_token::id(),
                            amount_in: amount_in as u64,
                            minimum_amount_out: 0,
                        }),
                        _ => None,
                    };

                    amount_in = destination_amount_swapped;
                    swap_arguments_vec.push(swap_arguments);
                }

                let profit = amount_in.saturating_sub(initial_amount) as u64;
                let first_pair_info = mev_path.path.first()?;
                let mint_pubkey = match first_pair_info.direction {
                    TradeDirection::AtoB => pool_states.0.get(&first_pair_info.pool)?.pool.pool_a_mint,
                    TradeDirection::BtoA => pool_states.0.get(&first_pair_info.pool)?.pool.pool_b_mint,
                };

                let minimum_profit = match self.minimum_profit.get(&mint_pubkey) {
                    Some(min_profit) => *min_profit,
                    None => {
                        warn!("[MEV] Token {} does not have a minimum profit set from config file.", mint_pubkey);
                        0u64
                    },
                };

                if profit < minimum_profit {
                    None
                } else if amount_in <= initial_amount {
                    // If the the `amount_in` is less than the initial amount, return
                    // `None`.
                    warn!("[MEV] The output amount is less than the initial amount, this shouldn't happen");
                    None
                } else {
                    let sanitized_tx_opt = swap_arguments_vec
                        .into_iter()
                        .collect::<Option<Vec<_>>>()
                        .and_then(|swap_args| {
                            Some(create_swap_tx(
                                swap_args,
                                blockhash,
                                self.user_authority.as_ref().as_ref()?,
                            ))
                        });

                    Some(MevTxOutput {
                        sanitized_tx: sanitized_tx_opt,
                        path_idx,
                        input_output_pairs,
                        profit,
                        marginal_price: path_output.marginal_price,
                    })
                }
            })
            .collect()
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

                Ok(MevMsg::ExecutedTransaction(executed_tx_output)) => writeln!(
                    file,
                    "{{\"event\":\"executed_transaction\",\"data\":{}}}",
                    serde_json::to_string(&executed_tx_output)
                        .expect("Constructed by us, should never fail")
                )
                .expect("[MEV] Could not write log executed transaction to file"),

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
                        program_id: Pubkey::from_str(
                            "9W959DqEETiGZocYWCQPaJ6sBmUzgfxXfqGeTEdp3aQP",
                        )
                        .unwrap(),
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
                        pool_mint: Pubkey::from_str("33k9G5HeH5JFukXTVxx3EmZrqjhb19Ej2GC2kqVPCKnM")
                            .unwrap(),
                        pool_fee: Pubkey::from_str("GqtosegQU4ad7W9AMHAQuuAFnjBQZ4VB4eZuPFrz8ALr")
                            .unwrap(),
                        pool_authority: authority_pubkey,
                        ..Default::default()
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
                    source_balance: None,
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
            'source_balance':null,\
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
