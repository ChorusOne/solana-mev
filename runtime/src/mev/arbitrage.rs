use log::warn;
use serde::Serialize;
use solana_sdk::{
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

use crate::inline_spl_token;

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
    pub input_output_pairs: Vec<InputOutputPairs>,
}

#[derive(Debug, PartialEq, Clone, Serialize)]
pub struct InputOutputPairs {
    pub token_in: u64,
    pub token_out: u64,
}

#[derive(Debug)]
pub struct MevTxOutput {
    // Not every MevTxOutput carries transactions, but we still want to log
    // them.
    pub sanitized_tx: Option<SanitizedTransaction>,
    // Index from the Path vector.
    pub path_idx: usize,
    pub input_output_pairs: Vec<InputOutputPairs>,
    pub profit: u64,
    // Marginal price when calculating the path's input.
    pub marginal_price: f64,
}

impl MevPath {
    fn get_mev_txs(
        &self,
        pool_states: &PoolStates,
        user_transfer_authority: Option<&Keypair>,
        blockhash: Hash,
        path_idx: usize,
    ) -> Option<MevTxOutput> {
        let (initial_amount, marginal_price) = self.get_input_amount_marginal_price(pool_states)?;
        let initial_amount = initial_amount.floor() as u128;
        let mut amount_in = initial_amount;
        let mut input_output_pairs = Vec::with_capacity(self.path.len());

        let mut swap_arguments_vec = Vec::with_capacity(self.path.len());
        for pair_info in &self.path {
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

        if amount_in <= initial_amount {
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
                        user_transfer_authority?,
                    ))
                });

            Some(MevTxOutput {
                sanitized_tx: sanitized_tx_opt,
                path_idx,
                input_output_pairs,
                profit: amount_in.saturating_sub(initial_amount) as u64,
                marginal_price,
            })
        }
    }

    /// Get (`input`, `marginal_price`), `input` is the input of the first hop
    /// of the path, and `marginal_price` is the multiplication of all fees and
    /// ratios from the path.
    fn get_input_amount_marginal_price(&self, pool_states: &PoolStates) -> Option<(f64, f64)> {
        let mut marginal_prices_acc = 1_f64;
        let mut optimal_input_denominator = 0_f64;
        let mut previous_ratio = 1_f64;
        let mut total_fee_acc = 1_f64;
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
            let ratio = token_balance_to / token_balance_from;
            marginal_prices_acc *= ratio;
            marginal_prices_acc *= total_fee;
            total_fee_acc *= total_fee;

            optimal_input_denominator += total_fee_acc * (previous_ratio / token_balance_from);
            previous_ratio = previous_ratio * ratio;
        }
        if marginal_prices_acc > 1_f64 {
            let optimal_input_numerator = marginal_prices_acc.sqrt() - 1_f64;
            let optimal_input = optimal_input_numerator / optimal_input_denominator;
            Some((optimal_input, marginal_prices_acc))
        } else {
            None
        }
    }
}

pub fn get_arbitrage_tx_outputs(
    mev_paths: &[MevPath],
    pool_states: &PoolStates,
    user_transfer_authority: Option<&Keypair>,
    blockhash: Hash,
) -> Vec<MevTxOutput> {
    mev_paths
        .into_iter()
        .enumerate()
        .filter_map(|(i, path)| {
            path.get_mev_txs(pool_states, user_transfer_authority, blockhash, i)
        })
        .collect()
}

struct SwapArguments {
    program_id: Pubkey,
    swap_pubkey: Pubkey,
    authority_pubkey: Pubkey,
    source_pubkey: Pubkey,
    swap_source_pubkey: Pubkey,
    swap_destination_pubkey: Pubkey,
    destination_pubkey: Pubkey,
    pool_mint_pubkey: Pubkey,
    pool_fee_pubkey: Pubkey,
    token_program: Pubkey,
    amount_in: u64,
    minimum_amount_out: u64,
}

fn create_swap_tx(
    swap_args_vec: Vec<SwapArguments>,
    blockhash: Hash,
    user_transfer_authority: &Keypair,
) -> SanitizedTransaction {
    let instructions: Vec<Instruction> = swap_args_vec
        .iter()
        .map(|swap_args| {
            let data = SwapInstruction::Swap(Swap {
                amount_in: swap_args.amount_in,
                minimum_amount_out: swap_args.minimum_amount_out,
            })
            .pack();

            let is_signer = false;
            let accounts = vec![
                AccountMeta::new_readonly(swap_args.swap_pubkey, is_signer),
                AccountMeta::new_readonly(swap_args.authority_pubkey, is_signer),
                AccountMeta::new_readonly(user_transfer_authority.pubkey(), true),
                AccountMeta::new(swap_args.source_pubkey, is_signer),
                AccountMeta::new(swap_args.swap_source_pubkey, is_signer),
                AccountMeta::new(swap_args.swap_destination_pubkey, is_signer),
                AccountMeta::new(swap_args.destination_pubkey, is_signer),
                AccountMeta::new(swap_args.pool_mint_pubkey, is_signer),
                AccountMeta::new(swap_args.pool_fee_pubkey, is_signer),
                AccountMeta::new_readonly(swap_args.token_program, is_signer),
            ];

            Instruction {
                program_id: swap_args.program_id,
                accounts,
                data,
            }
        })
        .collect();

    let signed_tx = Transaction::new_signed_with_payer(
        &instructions,
        Some(&user_transfer_authority.pubkey()),
        &[user_transfer_authority],
        blockhash,
    );

    SanitizedTransaction::try_from_legacy_transaction(signed_tx)
        .expect("Built by us, shouldn't fail.")
}

#[cfg(test)]
mod tests {
    use std::{str::FromStr, sync::Arc};

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
                            program_id: Pubkey::from_str(
                                "9W959DqEETiGZocYWCQPaJ6sBmUzgfxXfqGeTEdp3aQP",
                            )
                            .unwrap(),
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
                            program_id: Pubkey::from_str(
                                "9W959DqEETiGZocYWCQPaJ6sBmUzgfxXfqGeTEdp3aQP",
                            )
                            .unwrap(),
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
                            program_id: Pubkey::from_str(
                                "9W959DqEETiGZocYWCQPaJ6sBmUzgfxXfqGeTEdp3aQP",
                            )
                            .unwrap(),
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
        let arbs =
            get_arbitrage_tx_outputs(&[path.clone()], &pool_states, None, Hash::new_unique());
        assert_eq!(arbs[0].path_idx, 0);
        assert_eq!(
            arbs[0].input_output_pairs,
            vec![
                InputOutputPairs {
                    token_in: 4099483579,
                    token_out: 1799781506
                },
                InputOutputPairs {
                    token_in: 1799781506,
                    token_out: 6479400819484
                },
                InputOutputPairs {
                    token_in: 6479400819484,
                    token_out: 130347150790
                }
            ]
        );
        assert_eq!(arbs[0].marginal_price, 1010.9851646730779);
        assert_eq!(arbs[0].profit, 126247667211);

        let (input, marginal_price) = path.get_input_amount_marginal_price(&pool_states).unwrap();
        assert_eq!(marginal_price, 1010.9851646730779);
        assert_eq!(input, 4099483579.109189);

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

        let input_marginal_price_opt = path.get_input_amount_marginal_price(&pool_states);
        assert_eq!(input_marginal_price_opt, None);
        let arbs = get_arbitrage_tx_outputs(&[path], &pool_states, None, Hash::new_unique());
        assert!(arbs.is_empty());
    }

    #[test]
    fn test_serialize() {
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
                        program_id: Pubkey::from_str(
                            "9W959DqEETiGZocYWCQPaJ6sBmUzgfxXfqGeTEdp3aQP",
                        )
                        .unwrap(),
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
        let arbs = get_arbitrage_tx_outputs(&vec![], &pool_states, None, Hash::new_unique());
        assert!(arbs.is_empty());
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
                            program_id: Pubkey::from_str(
                                "9W959DqEETiGZocYWCQPaJ6sBmUzgfxXfqGeTEdp3aQP",
                            )
                            .unwrap(),
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
                            program_id: Pubkey::from_str(
                                "9W959DqEETiGZocYWCQPaJ6sBmUzgfxXfqGeTEdp3aQP",
                            )
                            .unwrap(),
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
                            program_id: Pubkey::from_str(
                                "9W959DqEETiGZocYWCQPaJ6sBmUzgfxXfqGeTEdp3aQP",
                            )
                            .unwrap(),
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
        let arbs = get_arbitrage_tx_outputs(&paths, &pool_states, None, Hash::new_unique());
        assert_eq!(arbs[0].path_idx, 0);
        assert_eq!(
            arbs[0].input_output_pairs,
            vec![
                InputOutputPairs {
                    token_in: 4099483579,
                    token_out: 1799781506
                },
                InputOutputPairs {
                    token_in: 1799781506,
                    token_out: 6479400819484
                },
                InputOutputPairs {
                    token_in: 6479400819484,
                    token_out: 130347150790
                }
            ]
        );
        assert_eq!(arbs[0].marginal_price, 1010.9851646730779);
        assert_eq!(arbs[0].profit, 126247667211);
    }
}
