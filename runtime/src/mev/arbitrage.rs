use std::str::FromStr;

use solana_sdk::pubkey::Pubkey;

use super::PrePostPoolStates;

enum TradeDirection {
    AtoB,
    BtoA,
}

pub struct PairInfo {
    pool: Pubkey,
    direction: TradeDirection,
}

struct MevPath(Vec<PairInfo>);

impl MevPath {
    fn get_arbitrage(&self, pre_post_pool_states: &PrePostPoolStates) -> bool {
        let mut total_rate = 1_f64;
        for pair_info in &self.0 {
            let tokens_state = pre_post_pool_states
                .orca_post_tx_pool
                .0
                .get(&pair_info.pool)
                .unwrap();

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
            let total_fee_denominator = fees.host_fee_denominator
                * fees.owner_trade_fee_denominator
                * fees.trade_fee_denominator;
            // 1 - fees
            let total_fee_numerator = total_fee_denominator - fees.host_fee_numerator
                + fees.host_fee_numerator
                    * (fees.owner_trade_fee_denominator * fees.trade_fee_denominator)
                + fees.owner_trade_fee_numerator
                    * (fees.host_fee_denominator * fees.trade_fee_denominator)
                + fees.trade_fee_numerator
                    * (fees.host_fee_denominator * fees.owner_trade_fee_denominator);

            let host_fee = fees.host_fee_numerator as f64 / fees.host_fee_denominator as f64;
            let owner_fee =
                fees.owner_trade_fee_numerator as f64 / fees.owner_trade_fee_denominator as f64;
            let trade_fee = fees.trade_fee_numerator as f64 / fees.trade_fee_denominator as f64;
            let total_fee = 1_f64 - (host_fee + owner_fee + trade_fee);

            total_rate *= token_balance_to / token_balance_from;
            total_rate *= total_fee;
            // numerator *= token_balance_to * total_fee_numerator;
            // denominator *= token_balance_from * total_fee_denominator;
        }

        if total_rate > 1_f64 {
            // optimal_input_numerator = (fee_ca * fee_bc * fee_ab * ratio_ab * ratio_bc * ratio_ca)**0.5 -1
            // let optimal_input_numerator = f64::sqrt(numerator) - 1;
            true
            // We have an arbitrage
        } else {
            false
        }
    }
}

fn get_all_arbitrage_from_path(pre_post_pool_states: &PrePostPoolStates) {
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

    let path = MevPath(vec![
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
    ]);

    let arbitrage_opportunity = path.get_arbitrage(pre_post_pool_states);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mev::{Fees, OrcaPoolAddresses, OrcaPoolWithBalance, PoolStates};
    use solana_sdk::{hash::Hash, signature::Signature};
    use std::collections::HashMap;

    #[test]
    fn test_get_arbitrage() {
        let pre_post_pool_states = PrePostPoolStates {
            transaction_hash: Hash::new_unique(),
            transaction_signature: Signature::new_unique(),
            slot: 1,
            orca_pre_tx_pool: PoolStates(HashMap::new()),
            orca_post_tx_pool: PoolStates(
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
            ),
        };
        let path = MevPath(vec![
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
        ]);

        let has_arbitrage = path.get_arbitrage(&pre_post_pool_states);
        assert_eq!(has_arbitrage, true);
    }
}
