use std::path::PathBuf;

use clap::Parser;
use solana_client::rpc_client::RpcClient;
use solana_program::pubkey::Pubkey;
use solana_sdk::{commitment_config::CommitmentConfig, signature::read_keypair_file};
use utils::create_token_pool;

mod utils;

#[derive(Parser, Debug)]
pub struct Opts {
    /// URL of cluster to connect to (e.g., https://api.devnet.solana.com for solana devnet)
    #[clap(long, default_value = "https://api.mainnet-beta.solana.com")]
    cluster: String,

    #[clap(long)]
    token_swap_program_id: Pubkey,

    #[clap(long, default_value = "~/.config/solana/id.json")]
    signer_path: PathBuf,

    #[clap(long)]
    token_a_account: Pubkey,
    #[clap(long)]
    token_b_account: Pubkey,

    #[clap(long, default_value = "0")]
    trade_fee_numerator: u64,
    #[clap(long, default_value = "100")]
    trade_fee_denominator: u64,
    #[clap(long, default_value = "0")]
    owner_trade_fee_numerator: u64,
    #[clap(long, default_value = "100")]
    owner_trade_fee_denominator: u64,
    #[clap(long, default_value = "0")]
    owner_withdraw_fee_numerator: u64,
    #[clap(long, default_value = "100")]
    owner_withdraw_fee_denominator: u64,
    #[clap(long, default_value = "0")]
    host_fee_numerator: u64,
    #[clap(long, default_value = "100")]
    host_fee_denominator: u64,
}

fn main() {
    let opts = Opts::parse();
    let rpc_client =
        RpcClient::new_with_commitment(opts.cluster.clone(), CommitmentConfig::confirmed());

    let fees = spl_token_swap::curve::fees::Fees {
        trade_fee_numerator: opts.trade_fee_numerator,
        trade_fee_denominator: opts.trade_fee_denominator,
        owner_trade_fee_numerator: opts.owner_trade_fee_numerator,
        owner_trade_fee_denominator: opts.owner_trade_fee_denominator,
        owner_withdraw_fee_numerator: opts.owner_withdraw_fee_numerator,
        owner_withdraw_fee_denominator: opts.owner_withdraw_fee_denominator,
        host_fee_numerator: opts.host_fee_numerator,
        host_fee_denominator: opts.host_fee_denominator,
    };

    let signer_keypair = read_keypair_file(opts.signer_path).unwrap();

    let token_pool = create_token_pool(
        &rpc_client,
        &signer_keypair,
        &opts.token_swap_program_id,
        &opts.token_a_account,
        &opts.token_b_account,
        fees,
    );
    println!("{}", serde_json::to_string(&token_pool).unwrap());
}
