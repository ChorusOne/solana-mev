use std::path::PathBuf;

use clap::{Parser, Subcommand};
use solana_client::rpc_client::RpcClient;
use solana_program::pubkey::Pubkey;
use solana_sdk::{commitment_config::CommitmentConfig, signature::read_keypair_file};
use utils::{create_token_pool, get_default_keypair_path, inner_swap};

use crate::utils::swap_tokens;

mod utils;

#[derive(Parser, Debug)]
pub struct Opts {
    /// URL of cluster to connect to (e.g., https://api.devnet.solana.com for solana devnet)
    #[clap(long, default_value = "http://localhost:8899")]
    cluster: String,

    #[clap(long)]
    token_swap_program_id: Pubkey,

    #[clap(long)]
    signer_path: Option<PathBuf>,

    #[clap(long)]
    token_swap_a_account: Pubkey,
    #[clap(long)]
    token_swap_b_account: Pubkey,

    #[clap(subcommand)]
    subcommand: OptSubcommand,
}

#[derive(Parser, Debug)]
struct InitializeTokenSwap {
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

#[derive(Parser, Debug)]
struct SwapTokens {
    #[clap(long)]
    token_swap_account: Pubkey,
    #[clap(long)]
    token_a_client: Pubkey,
    #[clap(long)]
    token_b_client: Pubkey,
    #[clap(long)]
    pool_mint: Pubkey,
    #[clap(long)]
    pool_fee: Pubkey,
    #[clap(long)]
    amount: u64,
    #[clap(long)]
    minimum_amount_out: u64,
}

#[derive(Parser, Debug)]
struct InnerSwap {
    #[clap(long)]
    caller_account: Pubkey,
    #[clap(long)]
    token_swap_account: Pubkey,
    #[clap(long)]
    token_a_client: Pubkey,
    #[clap(long)]
    token_b_client: Pubkey,
    #[clap(long)]
    pool_mint: Pubkey,
    #[clap(long)]
    pool_fee: Pubkey,
    #[clap(long)]
    amount: u64,
    #[clap(long)]
    minimum_amount_out: u64,
}

#[derive(Debug, Subcommand)]
enum OptSubcommand {
    Init(InitializeTokenSwap),
    Swap(SwapTokens),
    InnerSwap(InnerSwap),
}

fn main() {
    let opts = Opts::parse();
    let signer_path = opts.signer_path.unwrap_or(get_default_keypair_path());
    let rpc_client =
        RpcClient::new_with_commitment(opts.cluster.clone(), CommitmentConfig::confirmed());
    let signer_keypair = read_keypair_file(signer_path).unwrap();

    match opts.subcommand {
        OptSubcommand::Init(init_opts) => {
            let fees = spl_token_swap::curve::fees::Fees {
                trade_fee_numerator: init_opts.trade_fee_numerator,
                trade_fee_denominator: init_opts.trade_fee_denominator,
                owner_trade_fee_numerator: init_opts.owner_trade_fee_numerator,
                owner_trade_fee_denominator: init_opts.owner_trade_fee_denominator,
                owner_withdraw_fee_numerator: init_opts.owner_withdraw_fee_numerator,
                owner_withdraw_fee_denominator: init_opts.owner_withdraw_fee_denominator,
                host_fee_numerator: init_opts.host_fee_numerator,
                host_fee_denominator: init_opts.host_fee_denominator,
            };

            let token_pool = create_token_pool(
                &rpc_client,
                &signer_keypair,
                &opts.token_swap_program_id,
                &opts.token_swap_a_account,
                &opts.token_swap_b_account,
                fees,
            );
            println!("{}", serde_json::to_string(&token_pool).unwrap());
        }
        OptSubcommand::Swap(swap_opts) => {
            swap_tokens(
                &rpc_client,
                &signer_keypair,
                &opts.token_swap_program_id,
                &swap_opts.token_swap_account,
                &swap_opts.token_a_client,
                &opts.token_swap_a_account,
                &opts.token_swap_b_account,
                &swap_opts.token_b_client,
                &swap_opts.pool_mint,
                &swap_opts.pool_fee,
                swap_opts.amount,
                swap_opts.minimum_amount_out,
            );
        }
        OptSubcommand::InnerSwap(inner_swap_opts) => inner_swap(
            &rpc_client,
            &signer_keypair,
            &inner_swap_opts.caller_account,
            &opts.token_swap_program_id,
            &inner_swap_opts.token_swap_account,
            &inner_swap_opts.token_a_client,
            &opts.token_swap_a_account,
            &opts.token_swap_b_account,
            &inner_swap_opts.token_b_client,
            &inner_swap_opts.pool_mint,
            &inner_swap_opts.pool_fee,
            inner_swap_opts.amount,
            inner_swap_opts.minimum_amount_out,
        ),
    }
}
