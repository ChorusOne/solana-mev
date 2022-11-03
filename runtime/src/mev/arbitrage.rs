use std::sync::Arc;

use serde::Serialize;
use solana_sdk::{
    feature_set::FeatureSet,
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    transaction::{SanitizedTransaction, Transaction},
};
use spl_token_swap::{
    curve::calculator::SwapWithoutFeesResult,
    instruction::{Swap, SwapInstruction},
};

use crate::{
    ancestors::Ancestors,
    accounts::{Accounts, LoadedTransaction, MevAccounts},
    inline_spl_token,
    rent_collector::RentCollector,
    transaction_error_metrics::TransactionErrorMetrics,
};

use super::{
    utils::{deserialize_b58, serialize_b58},
    PoolStates,
};

#[derive(Debug, PartialEq, Clone, Deserialize, Serialize)]
pub enum TradeDirection {
    AtoB,
    BtoA,
}

#[derive(Debug, PartialEq, Clone, Deserialize, Serialize)]
pub struct PairInfo {
    #[serde(serialize_with = "serialize_b58")]
    #[serde(deserialize_with = "deserialize_b58")]
    pub pool: Pubkey,

    pub direction: TradeDirection,
}

#[derive(Debug, PartialEq, Clone, Deserialize, Serialize)]
pub struct MevPath {
    pub name: String,
    pub path: Vec<PairInfo>,
}

#[derive(Debug, PartialEq, Clone, Serialize)]
pub struct MevOpportunityWithInput<'a> {
    pub opportunity: &'a MevPath,
    pub input: f64,
}

impl MevPath {
    fn get_mev_txs(
        &self,
        program_id: Pubkey,
        pool_states: &PoolStates,
        user_transfer_authority: Option<&Keypair>,
        blockhash: Hash,
        ancestors: &Ancestors,
        fee: u64,
        error_counters: &mut TransactionErrorMetrics,
        rent_collector: &RentCollector,
        feature_set: &FeatureSet,
        mev_accounts_loaded_tx: Option<&(MevAccounts, LoadedTransaction)>,
        accounts: &Arc<Accounts>,
    ) -> Option<Vec<(SanitizedTransaction, LoadedTransaction)>> {
        let initial_amount = self.does_arbitrage_opportunity_exist(pool_states)?.ceil() as u128;
        let mut amount_in = initial_amount;
        let mut sanitized_txs = Vec::with_capacity(self.path.len());

        for pair_info in &self.path {
            let pool_state = pool_states.0.get(&pair_info.pool)?;

            let trade_fee = pool_state.fees.0.trading_fee(amount_in)?;
            let owner_fee = pool_state.fees.0.owner_trading_fee(amount_in)?;

            let total_fees = trade_fee.checked_add(owner_fee)?;

            let trade_direction = if pair_info.direction == TradeDirection::AtoB {
                spl_token_swap::curve::calculator::TradeDirection::AtoB
            } else {
                spl_token_swap::curve::calculator::TradeDirection::BtoA
            };

            let SwapWithoutFeesResult {
                source_amount_swapped,
                destination_amount_swapped,
            } = pool_state.curve_calculator.swap_without_fees(
                amount_in as u128,
                pool_state.pool_a_balance as u128,
                pool_state.pool_b_balance as u128,
                trade_direction,
            )?;

            let source_amount_swapped = source_amount_swapped.checked_add(total_fees)?;

            let swap_arguments = SwapArguments {
                program_id,
                swap_pubkey: pair_info.pool,
                authority_pubkey: pool_state.pool.pool_authority,
                user_transfer_authority: user_transfer_authority?,
                source_pubkey: pool_state.pool.source?,
                swap_source_pubkey: pool_state.pool.pool_a_account,
                swap_destination_pubkey: pool_state.pool.pool_b_account,
                destination_pubkey: pool_state.pool.destination?,
                pool_mint_pubkey: pool_state.pool.pool_mint,
                pool_fee_pubkey: pool_state.pool.pool_fee,
                token_program: inline_spl_token::id(),
                amount_in: amount_in as u64,
                minimum_amount_out: 0,
                blockhash,
            };

            let sanitized_tx = create_swap_tx(swap_arguments);

            let loaded_tx = accounts
                .load_transaction(
                    ancestors,
                    &sanitized_tx,
                    fee,
                    error_counters,
                    rent_collector,
                    feature_set,
                    None,
                    mev_accounts_loaded_tx,
                )
                .expect("Constructed by us, shouldn't fail");
            sanitized_txs.push((sanitized_tx, loaded_tx));

            amount_in = destination_amount_swapped;
        }
        if amount_in <= initial_amount {
            // If the the `amount_in` is less than the initial amount, return
            // `None`.
            None
        } else {
            Some(sanitized_txs)
        }
    }

    fn does_arbitrage_opportunity_exist(&self, pool_states: &PoolStates) -> Option<f64> {
        let mut marginal_prices_acc = 1_f64;
        let mut optimal_input_denominator = 0_f64;
        let mut previous_ratio = 1_f64;
        for pair_info in &self.path {
            let tokens_state = pool_states.0.get(&pair_info.pool)?;

            let (token_balance_from, token_balance_to) = match pair_info.direction {
                TradeDirection::AtoB => (
                    tokens_state.pool_a_balance as f64,
                    tokens_state.pool_b_balance as f64,
                ),
                TradeDirection::BtoA => (
                    tokens_state.pool_b_balance as f64,
                    tokens_state.pool_a_balance as f64,
                ),
            };
            let fees = &tokens_state.fees.0;
            let host_fee = if fees.host_fee_numerator == 0 {
                0_f64
            } else {
                fees.host_fee_numerator as f64 / fees.host_fee_denominator as f64
            };
            let owner_fee = if fees.owner_trade_fee_numerator == 0 {
                0_f64
            } else {
                fees.owner_trade_fee_numerator as f64 / fees.owner_trade_fee_denominator as f64
            };
            let trade_fee = if fees.trade_fee_numerator == 0 {
                0_f64
            } else {
                fees.trade_fee_numerator as f64 / fees.trade_fee_denominator as f64
            };

            let total_fee = 1_f64 - (host_fee + owner_fee + trade_fee);

            marginal_prices_acc *= token_balance_to / token_balance_from;
            marginal_prices_acc *= total_fee;

            optimal_input_denominator += total_fee * previous_ratio / token_balance_to;
            previous_ratio = token_balance_to / token_balance_from;
        }
        if marginal_prices_acc > 1_f64 {
            let optimal_input_numerator = marginal_prices_acc.sqrt() - 1_f64;
            let optimal_input = optimal_input_numerator / optimal_input_denominator;
            Some(optimal_input)
        } else {
            None
        }
    }
}

pub fn get_arbitrage_idxs(mev_paths: &[MevPath], pool_states: &PoolStates) -> Vec<(usize, f64)> {
    mev_paths
        .iter()
        .enumerate()
        .filter_map(|(i, path)| {
            path.does_arbitrage_opportunity_exist(pool_states)
                .map(|input| (i, input))
        })
        .collect()
}

struct SwapArguments<'a> {
    program_id: Pubkey,
    swap_pubkey: Pubkey,
    authority_pubkey: Pubkey,
    user_transfer_authority: &'a Keypair,
    source_pubkey: Pubkey,
    swap_source_pubkey: Pubkey,
    swap_destination_pubkey: Pubkey,
    destination_pubkey: Pubkey,
    pool_mint_pubkey: Pubkey,
    pool_fee_pubkey: Pubkey,
    token_program: Pubkey,
    amount_in: u64,
    minimum_amount_out: u64,
    blockhash: Hash,
}

fn create_swap_tx(swap_args: SwapArguments) -> SanitizedTransaction {
    let data = SwapInstruction::Swap(Swap {
        amount_in: swap_args.amount_in,
        minimum_amount_out: swap_args.minimum_amount_out,
    })
    .pack();

    let is_signer = false;
    let accounts = vec![
        AccountMeta::new_readonly(swap_args.swap_pubkey, is_signer),
        AccountMeta::new_readonly(swap_args.authority_pubkey, is_signer),
        AccountMeta::new_readonly(swap_args.user_transfer_authority.pubkey(), true),
        AccountMeta::new(swap_args.source_pubkey, is_signer),
        AccountMeta::new(swap_args.swap_source_pubkey, is_signer),
        AccountMeta::new(swap_args.swap_destination_pubkey, is_signer),
        AccountMeta::new(swap_args.destination_pubkey, is_signer),
        AccountMeta::new(swap_args.pool_mint_pubkey, is_signer),
        AccountMeta::new(swap_args.pool_fee_pubkey, is_signer),
        AccountMeta::new_readonly(swap_args.token_program, is_signer),
    ];

    let swap_ix = Instruction {
        program_id: swap_args.program_id,
        accounts,
        data,
    };

    let signed_tx = Transaction::new_signed_with_payer(
        &[swap_ix],
        Some(&swap_args.user_transfer_authority.pubkey()),
        &[swap_args.user_transfer_authority],
        swap_args.blockhash,
    );

    SanitizedTransaction::try_from_legacy_transaction(signed_tx)
        .expect("Built by us, shouldn't fail.")
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use spl_token_swap::curve::constant_product::ConstantProductCurve;

    use super::*;
    use crate::mev::{Fees, OrcaPoolAddresses, OrcaPoolWithBalance, PoolStates};

    #[test]
    fn test_get_arbitrage() {
        let curve_calculator = Arc::new(ConstantProductCurve::default());
        let mut pool_states = PoolStates(
            vec![
                (
                    Pubkey::from_str("v51xWrRwmFVH6EKe8eZTjgK5E4uC2tzY5sVt5cHbrkG").unwrap(),
                    OrcaPoolWithBalance {
                        pool: OrcaPoolAddresses {
                            address: Pubkey::from_str(
                                "v51xWrRwmFVH6EKe8eZTjgK5E4uC2tzY5sVt5cHbrkG",
                            )
                            .unwrap(),
                            pool_a_account: Pubkey::new_unique(),
                            pool_b_account: Pubkey::new_unique(),
                            source: None,
                            destination: None,
                            pool_mint: Pubkey::new_unique(),
                            pool_fee: Pubkey::new_unique(),
                            pool_authority: Pubkey::default(),
                        },
                        pool_a_balance: 4618233234,
                        pool_b_balance: 6400518033,
                        fees: Fees(spl_token_swap::curve::fees::Fees {
                            trade_fee_numerator: 25,
                            trade_fee_denominator: 10_000,
                            owner_trade_fee_numerator: 5,
                            owner_trade_fee_denominator: 10_000,
                            owner_withdraw_fee_numerator: 0,
                            owner_withdraw_fee_denominator: 1,
                            host_fee_numerator: 0,
                            host_fee_denominator: 1,
                        }),
                        curve_calculator: curve_calculator.clone(),
                    },
                ),
                (
                    Pubkey::from_str("B32UuhPSp6srSBbRTh4qZNjkegsehY9qXTwQgnPWYMZy").unwrap(),
                    OrcaPoolWithBalance {
                        pool: OrcaPoolAddresses {
                            address: Pubkey::from_str(
                                "B32UuhPSp6srSBbRTh4qZNjkegsehY9qXTwQgnPWYMZy",
                            )
                            .unwrap(),
                            pool_a_account: Pubkey::new_unique(),
                            pool_b_account: Pubkey::new_unique(),
                            source: None,
                            destination: None,
                            pool_mint: Pubkey::new_unique(),
                            pool_fee: Pubkey::new_unique(),
                            pool_authority: Pubkey::default(),
                        },
                        pool_a_balance: 54896627850684,
                        pool_b_balance: 13408494240,
                        fees: Fees(spl_token_swap::curve::fees::Fees {
                            trade_fee_numerator: 25,
                            trade_fee_denominator: 10_000,
                            owner_trade_fee_numerator: 5,
                            owner_trade_fee_denominator: 10_000,
                            owner_withdraw_fee_numerator: 0,
                            owner_withdraw_fee_denominator: 1,
                            host_fee_numerator: 0,
                            host_fee_denominator: 1,
                        }),
                        curve_calculator: curve_calculator.clone(),
                    },
                ),
                (
                    Pubkey::from_str("EfK84vYEKT1PoTJr6fBVKFbyA7ZoftfPo2LQPAJG1exL").unwrap(),
                    OrcaPoolWithBalance {
                        pool: OrcaPoolAddresses {
                            address: Pubkey::from_str(
                                "EfK84vYEKT1PoTJr6fBVKFbyA7ZoftfPo2LQPAJG1exL",
                            )
                            .unwrap(),
                            pool_a_account: Pubkey::new_unique(),
                            pool_b_account: Pubkey::new_unique(),
                            source: None,
                            destination: None,
                            pool_mint: Pubkey::new_unique(),
                            pool_fee: Pubkey::new_unique(),
                            pool_authority: Pubkey::default(),
                        },
                        pool_a_balance: 400881658679,
                        pool_b_balance: 138436018345,
                        fees: Fees(spl_token_swap::curve::fees::Fees {
                            trade_fee_numerator: 25,
                            trade_fee_denominator: 10_000,
                            owner_trade_fee_numerator: 5,
                            owner_trade_fee_denominator: 10_000,
                            owner_withdraw_fee_numerator: 0,
                            owner_withdraw_fee_denominator: 1,
                            host_fee_numerator: 0,
                            host_fee_denominator: 1,
                        }),
                        curve_calculator,
                    },
                ),
            ]
            .into_iter()
            .collect(),
        );
        let path = MevPath {
            name: "stETH->USDC->wstETH->stSOL->stSOL->USDC".to_owned(),
            path: vec![
                PairInfo {
                    pool: Pubkey::from_str("v51xWrRwmFVH6EKe8eZTjgK5E4uC2tzY5sVt5cHbrkG")
                        .expect("wstETH/USDC"),
                    direction: TradeDirection::BtoA,
                },
                PairInfo {
                    pool: Pubkey::from_str("B32UuhPSp6srSBbRTh4qZNjkegsehY9qXTwQgnPWYMZy")
                        .expect("stSOL/wstETH"),
                    direction: TradeDirection::BtoA,
                },
                PairInfo {
                    pool: Pubkey::from_str("EfK84vYEKT1PoTJr6fBVKFbyA7ZoftfPo2LQPAJG1exL")
                        .expect("stSOL/USDC"),
                    direction: TradeDirection::AtoB,
                },
            ],
        };
        let arb_idxs = get_arbitrage_idxs(&[path.clone()], &pool_states);
        assert_eq!(arb_idxs, vec![(0, 1036845732.6985222)]);

        let has_arbitrage = path.does_arbitrage_opportunity_exist(&pool_states);
        assert_eq!(has_arbitrage, Some(1036845732.6985222));

        pool_states
            .0
            .get_mut(&Pubkey::from_str("v51xWrRwmFVH6EKe8eZTjgK5E4uC2tzY5sVt5cHbrkG").unwrap())
            .unwrap()
            .pool_a_balance = 461823;
        pool_states
            .0
            .get_mut(&Pubkey::from_str("v51xWrRwmFVH6EKe8eZTjgK5E4uC2tzY5sVt5cHbrkG").unwrap())
            .unwrap()
            .pool_a_balance = 64005199;
        pool_states
            .0
            .get_mut(&Pubkey::from_str("B32UuhPSp6srSBbRTh4qZNjkegsehY9qXTwQgnPWYMZy").unwrap())
            .unwrap()
            .pool_a_balance = 5489662785068;
        pool_states
            .0
            .get_mut(&Pubkey::from_str("B32UuhPSp6srSBbRTh4qZNjkegsehY9qXTwQgnPWYMZy").unwrap())
            .unwrap()
            .pool_a_balance = 13408494240;
        pool_states
            .0
            .get_mut(&Pubkey::from_str("EfK84vYEKT1PoTJr6fBVKFbyA7ZoftfPo2LQPAJG1exL").unwrap())
            .unwrap()
            .pool_a_balance = 40088165867986;
        pool_states
            .0
            .get_mut(&Pubkey::from_str("EfK84vYEKT1PoTJr6fBVKFbyA7ZoftfPo2LQPAJG1exL").unwrap())
            .unwrap()
            .pool_a_balance = 1384360183450;

        let has_arbitrage = path.does_arbitrage_opportunity_exist(&pool_states);
        assert_eq!(has_arbitrage, None);
        let arb_idxs = get_arbitrage_idxs(&[path], &pool_states);
        assert_eq!(arb_idxs, vec![]);
    }

    #[test]
    fn test_serialize() {
        let curve_calculator = Arc::new(ConstantProductCurve::default());
        let path = MevPath {
            name: "SOL->USDC->wstETH->stSOL->stSOL->USDC->SOL".to_owned(),
            path: vec![
                PairInfo {
                    pool: Pubkey::from_str("EGZ7tiLeH62TPV1gL8WwbXGzEPa9zmcpVnnkPKKnrE2U")
                        .expect("Known SOL/USDC pool address"),
                    direction: TradeDirection::AtoB,
                },
                PairInfo {
                    pool: Pubkey::from_str("v51xWrRwmFVH6EKe8eZTjgK5E4uC2tzY5sVt5cHbrkG")
                        .expect("Known wstETH/USDC address"),
                    direction: TradeDirection::BtoA,
                },
                PairInfo {
                    pool: Pubkey::from_str("B32UuhPSp6srSBbRTh4qZNjkegsehY9qXTwQgnPWYMZy")
                        .expect("Known stSOL/wstETH address"),
                    direction: TradeDirection::BtoA,
                },
                PairInfo {
                    pool: Pubkey::from_str("EfK84vYEKT1PoTJr6fBVKFbyA7ZoftfPo2LQPAJG1exL")
                        .expect("Known stSOL/USDC address"),
                    direction: TradeDirection::AtoB,
                },
                PairInfo {
                    pool: Pubkey::from_str("EGZ7tiLeH62TPV1gL8WwbXGzEPa9zmcpVnnkPKKnrE2U")
                        .expect("Known SOL/USDC pool address"),
                    direction: TradeDirection::BtoA,
                },
            ],
        };
        let expected_result = "{\
            'name':'SOL->USDC->wstETH->stSOL->stSOL->USDC->SOL',\
            'path':[\
            {'pool':'EGZ7tiLeH62TPV1gL8WwbXGzEPa9zmcpVnnkPKKnrE2U','direction':'AtoB'},\
            {'pool':'v51xWrRwmFVH6EKe8eZTjgK5E4uC2tzY5sVt5cHbrkG','direction':'BtoA'},\
            {'pool':'B32UuhPSp6srSBbRTh4qZNjkegsehY9qXTwQgnPWYMZy','direction':'BtoA'},\
            {'pool':'EfK84vYEKT1PoTJr6fBVKFbyA7ZoftfPo2LQPAJG1exL','direction':'AtoB'},\
            {'pool':'EGZ7tiLeH62TPV1gL8WwbXGzEPa9zmcpVnnkPKKnrE2U','direction':'BtoA'}]}"
            .replace("'", "\"");
        assert_eq!(serde_json::to_string(&path).unwrap(), expected_result);
    }

    #[test]
    fn get_opportunity_with_empty_paths() {
        let curve_calculator = Arc::new(ConstantProductCurve::default());
        let pool_states = PoolStates(
            vec![(
                Pubkey::from_str("v51xWrRwmFVH6EKe8eZTjgK5E4uC2tzY5sVt5cHbrkG").unwrap(),
                OrcaPoolWithBalance {
                    pool: OrcaPoolAddresses {
                        address: Pubkey::from_str("v51xWrRwmFVH6EKe8eZTjgK5E4uC2tzY5sVt5cHbrkG")
                            .unwrap(),
                        pool_a_account: Pubkey::new_unique(),
                        pool_b_account: Pubkey::new_unique(),
                        source: None,
                        destination: None,
                        pool_mint: Pubkey::new_unique(),
                        pool_fee: Pubkey::new_unique(),
                        pool_authority: Pubkey::default(),
                    },
                    pool_a_balance: 4618233234,
                    pool_b_balance: 6400518033,
                    fees: Fees(spl_token_swap::curve::fees::Fees {
                        trade_fee_numerator: 25,
                        trade_fee_denominator: 10_000,
                        owner_trade_fee_numerator: 5,
                        owner_trade_fee_denominator: 10_000,
                        owner_withdraw_fee_numerator: 0,
                        owner_withdraw_fee_denominator: 1,
                        host_fee_numerator: 0,
                        host_fee_denominator: 1,
                    }),
                    curve_calculator,
                },
            )]
            .into_iter()
            .collect(),
        );
        let arbs = get_arbitrage_idxs(&vec![], &pool_states);
        assert_eq!(arbs, vec![]);
    }

    #[test]
    fn get_opportunity_exists_when_other_does_not() {
        let curve_calculator = Arc::new(ConstantProductCurve::default());
        let pool_states = PoolStates(
            vec![
                (
                    Pubkey::from_str("v51xWrRwmFVH6EKe8eZTjgK5E4uC2tzY5sVt5cHbrkG").unwrap(),
                    OrcaPoolWithBalance {
                        pool: OrcaPoolAddresses {
                            address: Pubkey::from_str(
                                "v51xWrRwmFVH6EKe8eZTjgK5E4uC2tzY5sVt5cHbrkG",
                            )
                            .unwrap(),
                            pool_a_account: Pubkey::new_unique(),
                            pool_b_account: Pubkey::new_unique(),
                            source: None,
                            destination: None,
                            pool_mint: Pubkey::new_unique(),
                            pool_fee: Pubkey::new_unique(),
                            pool_authority: Pubkey::default(),
                        },
                        pool_a_balance: 4618233234,
                        pool_b_balance: 6400518033,
                        fees: Fees(spl_token_swap::curve::fees::Fees {
                            trade_fee_numerator: 25,
                            trade_fee_denominator: 10_000,
                            owner_trade_fee_numerator: 5,
                            owner_trade_fee_denominator: 10_000,
                            owner_withdraw_fee_numerator: 0,
                            owner_withdraw_fee_denominator: 1,
                            host_fee_numerator: 0,
                            host_fee_denominator: 1,
                        }),
                        curve_calculator: curve_calculator.clone(),
                    },
                ),
                (
                    Pubkey::from_str("B32UuhPSp6srSBbRTh4qZNjkegsehY9qXTwQgnPWYMZy").unwrap(),
                    OrcaPoolWithBalance {
                        pool: OrcaPoolAddresses {
                            address: Pubkey::from_str(
                                "B32UuhPSp6srSBbRTh4qZNjkegsehY9qXTwQgnPWYMZy",
                            )
                            .unwrap(),
                            pool_a_account: Pubkey::new_unique(),
                            pool_b_account: Pubkey::new_unique(),
                            source: None,
                            destination: None,
                            pool_mint: Pubkey::new_unique(),
                            pool_fee: Pubkey::new_unique(),
                            pool_authority: Pubkey::default(),
                        },
                        pool_a_balance: 54896627850684,
                        pool_b_balance: 13408494240,
                        fees: Fees(spl_token_swap::curve::fees::Fees {
                            trade_fee_numerator: 25,
                            trade_fee_denominator: 10_000,
                            owner_trade_fee_numerator: 5,
                            owner_trade_fee_denominator: 10_000,
                            owner_withdraw_fee_numerator: 0,
                            owner_withdraw_fee_denominator: 1,
                            host_fee_numerator: 0,
                            host_fee_denominator: 1,
                        }),
                        curve_calculator: curve_calculator.clone(),
                    },
                ),
                (
                    Pubkey::from_str("EfK84vYEKT1PoTJr6fBVKFbyA7ZoftfPo2LQPAJG1exL").unwrap(),
                    OrcaPoolWithBalance {
                        pool: OrcaPoolAddresses {
                            address: Pubkey::from_str(
                                "EfK84vYEKT1PoTJr6fBVKFbyA7ZoftfPo2LQPAJG1exL",
                            )
                            .unwrap(),
                            pool_a_account: Pubkey::new_unique(),
                            pool_b_account: Pubkey::new_unique(),
                            source: None,
                            destination: None,
                            pool_mint: Pubkey::new_unique(),
                            pool_fee: Pubkey::new_unique(),
                            pool_authority: Pubkey::default(),
                        },
                        pool_a_balance: 400881658679,
                        pool_b_balance: 138436018345,
                        fees: Fees(spl_token_swap::curve::fees::Fees {
                            trade_fee_numerator: 25,
                            trade_fee_denominator: 10_000,
                            owner_trade_fee_numerator: 5,
                            owner_trade_fee_denominator: 10_000,
                            owner_withdraw_fee_numerator: 0,
                            owner_withdraw_fee_denominator: 1,
                            host_fee_numerator: 0,
                            host_fee_denominator: 1,
                        }),
                        curve_calculator,
                    },
                ),
            ]
            .into_iter()
            .collect(),
        );
        let paths = vec![
            MevPath {
                name: "stETH->USDC->wstETH->stSOL->stSOL->USDC".to_owned(),
                path: vec![
                    PairInfo {
                        pool: Pubkey::from_str("v51xWrRwmFVH6EKe8eZTjgK5E4uC2tzY5sVt5cHbrkG")
                            .expect("wstETH/USDC"),
                        direction: TradeDirection::BtoA,
                    },
                    PairInfo {
                        pool: Pubkey::from_str("B32UuhPSp6srSBbRTh4qZNjkegsehY9qXTwQgnPWYMZy")
                            .expect("stSOL/wstETH"),
                        direction: TradeDirection::BtoA,
                    },
                    PairInfo {
                        pool: Pubkey::from_str("EfK84vYEKT1PoTJr6fBVKFbyA7ZoftfPo2LQPAJG1exL")
                            .expect("stSOL/USDC"),
                        direction: TradeDirection::AtoB,
                    },
                ],
            },
            MevPath {
                name: "stSOL->USDC".to_owned(),
                path: vec![PairInfo {
                    pool: Pubkey::from_str("EfK84vYEKT1PoTJr6fBVKFbyA7ZoftfPo2LQPAJG1exL")
                        .expect("stSOL/USDC"),
                    direction: TradeDirection::AtoB,
                }],
            },
        ];
        let arb_idxs = get_arbitrage_idxs(&paths, &pool_states);
        assert_eq!(arb_idxs, vec![(0, 1036845732.6985222)]);
    }
}
