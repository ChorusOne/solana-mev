use serde::Serialize;
use solana_sdk::{
    hash::Hash,
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::Keypair,
    signer::Signer,
    transaction::{SanitizedTransaction, Transaction},
};
use spl_token_swap::instruction::{Swap, SwapInstruction};

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

pub struct PathCalculationOutput {
    pub optimal_input: f64,
    pub marginal_price: f64,
    pub source_token_balance: Option<u64>,
}

impl MevPath {
    /// Get (`input`, `marginal_price`), `input` is the input of the first hop
    /// of the path, and `marginal_price` is the multiplication of all fees and
    /// ratios from the path.
    pub fn get_path_calculation_output(
        &self,
        pool_states: &PoolStates,
    ) -> Option<PathCalculationOutput> {
        let mut marginal_prices_acc = 1_f64;
        let mut optimal_input_denominator = 0_f64;
        let mut previous_ratio = 1_f64;
        let mut total_fee_acc = 1_f64;

        let source_amount = pool_states.0.get(&self.path.first()?.pool)?.source_balance;
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
            Some(PathCalculationOutput {
                optimal_input,
                marginal_price: marginal_prices_acc,
                source_token_balance: source_amount,
            })
        } else {
            None
        }
    }
}

pub struct SwapArguments {
    pub program_id: Pubkey,
    pub swap_pubkey: Pubkey,
    pub authority_pubkey: Pubkey,
    pub source_pubkey: Pubkey,
    pub swap_source_pubkey: Pubkey,
    pub swap_destination_pubkey: Pubkey,
    pub destination_pubkey: Pubkey,
    pub pool_mint_pubkey: Pubkey,
    pub pool_fee_pubkey: Pubkey,
    pub token_program: Pubkey,
    pub amount_in: u64,
    pub minimum_amount_out: u64,
}

pub fn create_swap_tx(
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
    use std::{collections::HashMap, path::PathBuf, str::FromStr, sync::Arc};

    use spl_token_swap::curve::constant_product::ConstantProductCurve;
    use tempfile::NamedTempFile;

    use super::*;
    use crate::mev::{
        utils::{AllOrcaPoolAddresses, MevConfig},
        Fees, Mev, MevLog, OrcaPoolAddresses, OrcaPoolWithBalance, PoolStates,
    };

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
                            ..Default::default()
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
                        source_balance: None,
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
                            ..Default::default()
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
                        source_balance: None,
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
                            ..Default::default()
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
                        source_balance: None,
                    },
                ),
            ]
            .into_iter()
            .collect(),
        );
        let path = MevPath {
            name: "USDC->stETH->stSOL->USDC".to_owned(),
            path: vec![
                PairInfo {
                    pool: Pubkey::from_str("v51xWrRwmFVH6EKe8eZTjgK5E4uC2tzY5sVt5cHbrkG")
                        .expect("stETH/USDC"),
                    direction: TradeDirection::BtoA,
                },
                PairInfo {
                    pool: Pubkey::from_str("B32UuhPSp6srSBbRTh4qZNjkegsehY9qXTwQgnPWYMZy")
                        .expect("stSOL/stETH"),
                    direction: TradeDirection::BtoA,
                },
                PairInfo {
                    pool: Pubkey::from_str("EfK84vYEKT1PoTJr6fBVKFbyA7ZoftfPo2LQPAJG1exL")
                        .expect("stSOL/USDC"),
                    direction: TradeDirection::AtoB,
                },
            ],
        };
        let mev_config = MevConfig {
            log_path: PathBuf::from(NamedTempFile::new().unwrap().path().to_str().unwrap()),
            watched_programs: vec![],
            orca_accounts: AllOrcaPoolAddresses(vec![]),
            mev_paths: vec![path],
            user_authority_path: None,
            minimum_profit: HashMap::new(),
        };
        let mev_log = MevLog::new(&mev_config);
        let mev = Mev::new(mev_log.log_send_channel.clone(), mev_config);
        let arbs = mev.get_arbitrage_tx_outputs(&pool_states, Hash::new_unique());
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
                },
            ],
        );
        assert_eq!(arbs[0].marginal_price, 1010.9851646730779);
        assert_eq!(arbs[0].profit, 126247667211);

        let path_output = mev
            .mev_paths
            .first()
            .unwrap()
            .get_path_calculation_output(&pool_states)
            .unwrap();
        assert_eq!(path_output.marginal_price, 1010.9851646730779);
        assert_eq!(path_output.optimal_input, 4099483579.109189);

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

        let path_output = mev
            .mev_paths
            .first()
            .unwrap()
            .get_path_calculation_output(&pool_states);
        assert!(path_output.is_none());
        let arbs = mev.get_arbitrage_tx_outputs(&pool_states, Hash::new_unique());
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
                        ..Default::default()
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
                    source_balance: None,
                },
            )]
            .into_iter()
            .collect(),
        );
        let mev_config = MevConfig {
            log_path: PathBuf::from(NamedTempFile::new().unwrap().path().to_str().unwrap()),
            watched_programs: vec![],
            orca_accounts: AllOrcaPoolAddresses(vec![]),
            mev_paths: vec![],
            user_authority_path: None,
            minimum_profit: HashMap::new(),
        };
        let mev_log = MevLog::new(&mev_config);
        let mev = Mev::new(mev_log.log_send_channel.clone(), mev_config);
        let arbs = mev.get_arbitrage_tx_outputs(&pool_states, Hash::new_unique());
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
                            ..Default::default()
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
                        source_balance: None,
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
                            ..Default::default()
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
                        source_balance: None,
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
                            ..Default::default()
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
                        source_balance: None,
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
                path: vec![
                    PairInfo {
                        pool: Pubkey::from_str("EfK84vYEKT1PoTJr6fBVKFbyA7ZoftfPo2LQPAJG1exL")
                            .expect("stSOL/USDC"),
                        direction: TradeDirection::AtoB,
                    },
                    PairInfo {
                        pool: Pubkey::from_str("EfK84vYEKT1PoTJr6fBVKFbyA7ZoftfPo2LQPAJG1exL")
                            .expect("stSOL/USDC"),
                        direction: TradeDirection::BtoA,
                    },
                ],
            },
        ];

        let mev_config = MevConfig {
            log_path: PathBuf::from(NamedTempFile::new().unwrap().path().to_str().unwrap()),
            watched_programs: vec![],
            orca_accounts: AllOrcaPoolAddresses(vec![]),
            mev_paths: paths,
            user_authority_path: None,
            minimum_profit: HashMap::new(),
        };
        let mev_log = MevLog::new(&mev_config);
        let mev = Mev::new(mev_log.log_send_channel.clone(), mev_config);

        let arbs = mev.get_arbitrage_tx_outputs(&pool_states, Hash::new_unique());
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

    #[test]
    #[should_panic]
    fn path_with_one_pool_with_same_direction_should_panic() {
        let paths = vec![MevPath {
            name: "stETH->USDC->wstETH".to_owned(),
            path: vec![
                PairInfo {
                    pool: Pubkey::from_str("v51xWrRwmFVH6EKe8eZTjgK5E4uC2tzY5sVt5cHbrkG")
                        .expect("wstETH/USDC"),
                    direction: TradeDirection::BtoA,
                },
                PairInfo {
                    pool: Pubkey::from_str("v51xWrRwmFVH6EKe8eZTjgK5E4uC2tzY5sVt5cHbrkG")
                        .expect("wstETH/USDC"),
                    direction: TradeDirection::BtoA,
                },
            ],
        }];

        let mev_config = MevConfig {
            log_path: PathBuf::from(NamedTempFile::new().unwrap().path().to_str().unwrap()),
            watched_programs: vec![],
            orca_accounts: AllOrcaPoolAddresses(vec![]),
            mev_paths: paths,
            user_authority_path: None,
            minimum_profit: HashMap::new(),
        };
        let mev_log = MevLog::new(&mev_config);
        let _mev = Mev::new(mev_log.log_send_channel.clone(), mev_config);
    }
}
