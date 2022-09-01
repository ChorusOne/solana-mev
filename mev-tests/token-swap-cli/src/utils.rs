use std::sync::Arc;

use serde::Serialize;
use solana_client::rpc_client::RpcClient;
use solana_program::{instruction::Instruction, rent::Rent, system_instruction, sysvar};
use solana_sdk::{signature::Keypair, signer::Signer, signers::Signers, transaction::Transaction};
use spl_token::solana_program::{program_pack::Pack, pubkey::Pubkey};
use spl_token_swap::curve::{
    base::{CurveType, SwapCurve},
    constant_product::ConstantProductCurve,
    fees::Fees,
};

pub fn get_rent(rpc_client: &RpcClient) -> Rent {
    let account = rpc_client.get_account(&sysvar::rent::id()).unwrap();
    bincode::deserialize(&account.data).unwrap()
}

/// Push instructions to create and initialize and SPL token mint.
///
/// This uses the default number of decimals: 9. Returns the mint address.
pub fn push_create_spl_token_mint(
    signer: &Keypair,
    rpc_client: &RpcClient,
    instructions: &mut Vec<Instruction>,
    mint_authority: &Pubkey,
) -> Keypair {
    let rent = get_rent(&rpc_client);
    let min_rent = rent.minimum_balance(spl_token::state::Mint::LEN);

    let keypair = Keypair::new();

    instructions.push(system_instruction::create_account(
        &signer.pubkey(),
        &keypair.pubkey(),
        // Deposit enough SOL to make it rent-exempt.
        min_rent,
        spl_token::state::Mint::LEN as u64,
        // The new account should be owned by the SPL token program.
        &spl_token::id(),
    ));

    let num_decimals = 9;
    assert_eq!(spl_token::native_mint::DECIMALS, num_decimals);
    let freeze_authority = None;

    instructions.push(
        spl_token::instruction::initialize_mint(
            &spl_token::id(),
            &keypair.pubkey(),
            mint_authority,
            freeze_authority,
            num_decimals,
        )
        .unwrap(),
    );

    keypair
}

/// Push instructions to create and initialize an SPL token account.
///
/// Returns the keypair for the account. This keypair needs to sign the
/// transaction.
pub fn push_create_spl_token_account(
    signer: &Keypair,
    rpc_client: &RpcClient,
    instructions: &mut Vec<Instruction>,
    mint: &Pubkey,
    owner: &Pubkey,
) -> Keypair {
    let rent = get_rent(&rpc_client);
    let min_rent = rent.minimum_balance(spl_token::state::Account::LEN);

    let keypair = Keypair::new();

    instructions.push(system_instruction::create_account(
        &signer.pubkey(),
        &keypair.pubkey(),
        // Deposit enough SOL to make it rent-exempt.
        min_rent,
        spl_token::state::Account::LEN as u64,
        // The new account should be owned by the SPL token program.
        &spl_token::id(),
    ));
    instructions.push(
        spl_token::instruction::initialize_account(
            &spl_token::id(),
            &keypair.pubkey(),
            mint,
            owner,
        )
        .unwrap(),
    );

    keypair
}

pub fn sign_and_send_transaction<T: Signers>(
    signer: &Keypair,
    rpc_client: &RpcClient,
    instructions: &[Instruction],
    signers: &T,
) -> Transaction {
    let mut tx = Transaction::new_with_payer(instructions, Some(&signer.pubkey()));
    let recent_blockhash = rpc_client.get_latest_blockhash().unwrap();
    tx.try_sign(signers, recent_blockhash).unwrap();
    rpc_client.send_and_confirm_transaction(&tx).unwrap();
    tx
}

#[derive(Serialize)]
pub struct TokenPool {
    address: Pubkey,
}

pub fn create_token_pool(
    rpc_client: &RpcClient,
    signer_keypair: &Keypair,
    token_swap_program_id: &Pubkey,
    token_a_account: &Pubkey,
    token_b_account: &Pubkey,
    fees: Fees,
) -> TokenPool {
    let mut instructions = Vec::new();

    let token_pool_account = Keypair::new();

    let rent = get_rent(&rpc_client);
    let rent_lamports = rent.minimum_balance(spl_token_swap::state::SwapVersion::LATEST_LEN);

    instructions.push(system_instruction::create_account(
        &signer_keypair.pubkey(),
        &token_pool_account.pubkey(),
        rent_lamports,
        spl_token_swap::state::SwapVersion::LATEST_LEN as u64,
        &token_swap_program_id,
    ));

    let (authority_pubkey, _authority_bump_seed) = Pubkey::find_program_address(
        &[&token_pool_account.pubkey().to_bytes()[..]],
        &token_swap_program_id,
    );

    let pool_mint_keypair = push_create_spl_token_mint(
        &signer_keypair,
        &rpc_client,
        &mut instructions,
        &authority_pubkey,
    );
    let pool_mint_pubkey = pool_mint_keypair.pubkey();
    let pool_fee_keypair = push_create_spl_token_account(
        &signer_keypair,
        &rpc_client,
        &mut instructions,
        &pool_mint_pubkey,
        &signer_keypair.pubkey(),
    );
    let pool_token_keypair = push_create_spl_token_account(
        &signer_keypair,
        &rpc_client,
        &mut instructions,
        &pool_mint_pubkey,
        &signer_keypair.pubkey(),
    );

    // Change the token owner to the pool's authority.
    instructions.push(
        spl_token::instruction::set_authority(
            &spl_token::id(),
            &token_a_account,
            Some(&authority_pubkey),
            spl_token::instruction::AuthorityType::AccountOwner,
            &signer_keypair.pubkey(),
            &[],
        )
        .unwrap(),
    );

    // Change the token owner to the pool's authority.
    instructions.push(
        spl_token::instruction::set_authority(
            &spl_token::id(),
            &token_b_account,
            Some(&authority_pubkey),
            spl_token::instruction::AuthorityType::AccountOwner,
            &signer_keypair.pubkey(),
            &[],
        )
        .unwrap(),
    );

    let signers = vec![
        signer_keypair,
        &token_pool_account,
        &pool_mint_keypair,
        &pool_fee_keypair,
        &pool_token_keypair,
    ];

    let swap_curve = SwapCurve {
        curve_type: CurveType::ConstantProduct,
        calculator: Arc::new(ConstantProductCurve),
    };

    let initialize_pool_instruction = spl_token_swap::instruction::initialize(
        &token_swap_program_id,
        &spl_token::id(),
        &token_pool_account.pubkey(),
        &authority_pubkey,
        &token_a_account,
        &token_b_account,
        &pool_mint_pubkey,
        &pool_fee_keypair.pubkey(),
        &pool_token_keypair.pubkey(),
        fees,
        swap_curve,
    )
    .expect("Failed to create token pool initialization instruction.");
    instructions.push(initialize_pool_instruction);
    sign_and_send_transaction(&signer_keypair, &rpc_client, &instructions[..], &signers);

    TokenPool {
        address: token_pool_account.pubkey(),
    }
}
