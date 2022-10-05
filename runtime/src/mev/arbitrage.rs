use std::str::FromStr;

use serde::Serialize;
use solana_sdk::pubkey::Pubkey;

use super::{
    utils::{deserialize_b58, serialize_b58},
    PoolStates,
};

#[derive(Debug, PartialEq, Deserialize, Serialize)]
pub enum TradeDirection {
    AtoB,
    BtoA,
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
pub struct PairInfo {
    #[serde(serialize_with = "serialize_b58")]
    #[serde(deserialize_with = "deserialize_b58")]
    pub pool: Pubkey,

    pub direction: TradeDirection,
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
pub struct MevPath {
    pub path: Vec<PairInfo>,
}

impl MevPath {
    fn does_arbitrage_opportunity_exist(&self, pool_states: &PoolStates) -> Option<()> {
        let mut marginal_prices_acc = 1_f64;
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
            let host_fee = fees.host_fee_numerator as f64 / fees.host_fee_denominator as f64;
            let owner_fee =
                fees.owner_trade_fee_numerator as f64 / fees.owner_trade_fee_denominator as f64;
            let trade_fee = fees.trade_fee_numerator as f64 / fees.trade_fee_denominator as f64;
            let total_fee = 1_f64 - (host_fee + owner_fee + trade_fee);

            marginal_prices_acc *= token_balance_to / token_balance_from;
            marginal_prices_acc *= total_fee;
        }

        if marginal_prices_acc > 1_f64 {
            Some(())
        } else {
            None
        }
    }
}

/// Hard-code some arbitrages and return if there's an opportunity or not
pub fn get_pre_defined_arbitrage_path(pre_post_pool_states: &PoolStates) -> Option<MevPath> {
    // wSOL->USDC->wstETH->stSOL->USDC->wSOL
    // wSOL/USDC: EGZ7tiLeH62TPV1gL8WwbXGzEPa9zmcpVnnkPKKnrE2U
    // wstETH/USDC: v51xWrRwmFVH6EKe8eZTjgK5E4uC2tzY5sVt5cHbrkG
    // stSOL/wstETH: B32UuhPSp6srSBbRTh4qZNjkegsehY9qXTwQgnPWYMZy
    // stSOL/USDC: EfK84vYEKT1PoTJr6fBVKFbyA7ZoftfPo2LQPAJG1exL
    // wSOL/USDC: EGZ7tiLeH62TPV1gL8WwbXGzEPa9zmcpVnnkPKKnrE2U
    //
    // Decimals: wSOL: 9
    // USDC: 6
    // wstETH: 8
    // stSOL: 9

    let path = MevPath {
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

    path.does_arbitrage_opportunity_exist(pre_post_pool_states)
        .and(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mev::{Fees, OrcaPoolAddresses, OrcaPoolWithBalance, PoolStates};

    #[test]
    fn test_get_arbitrage() {
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
                    },
                ),
            ]
            .into_iter()
            .collect(),
        );
        let path = MevPath {
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

        let has_arbitrage = path.does_arbitrage_opportunity_exist(&pool_states);
        assert_eq!(has_arbitrage, Some(()));

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
    }

    #[test]
    fn test_serialize() {
        let path = MevPath {
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
        let expected_result = "{'path':[\
            {'pool':'EGZ7tiLeH62TPV1gL8WwbXGzEPa9zmcpVnnkPKKnrE2U','direction':'AtoB'},\
            {'pool':'v51xWrRwmFVH6EKe8eZTjgK5E4uC2tzY5sVt5cHbrkG','direction':'BtoA'},\
            {'pool':'B32UuhPSp6srSBbRTh4qZNjkegsehY9qXTwQgnPWYMZy','direction':'BtoA'},\
            {'pool':'EfK84vYEKT1PoTJr6fBVKFbyA7ZoftfPo2LQPAJG1exL','direction':'AtoB'},\
            {'pool':'EGZ7tiLeH62TPV1gL8WwbXGzEPa9zmcpVnnkPKKnrE2U','direction':'BtoA'}]}"
            .replace("'", "\"");
        assert_eq!(serde_json::to_string(&path).unwrap(), expected_result);
    }
}
