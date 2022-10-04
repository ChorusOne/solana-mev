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
    fn get_arbitrage(&self, pre_post_pool_state: &PrePostPoolStates) {
        let mut numerator = 1;
        let mut denominator = 1;
        for pair_info in &self.0 {
            let tokens_state = pre_post_pool_state
                .orca_post_tx_pool
                .0
                .get(&pair_info.pool)
                .unwrap();

            let (token_balance_from, token_balance_to) = match pair_info.direction {
                TradeDirection::AtoB => (tokens_state.pool_a_balance, tokens_state.pool_b_balance),
                TradeDirection::BtoA => (tokens_state.pool_b_balance, tokens_state.pool_a_balance),
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

            numerator *= token_balance_to * total_fee_numerator;
            denominator *= token_balance_from * total_fee_denominator;
        }

        if numerator > denominator {
            // We have an arbitrage
        }
    }
}

fn get_all_arbitrage_from_path(pre_post_pool_state: &PrePostPoolStates) {
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

    let arbitrage_opportunity = path.get_arbitrage(pre_post_pool_state);
}
