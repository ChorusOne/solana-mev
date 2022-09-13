use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{
    account_info::{next_account_info, AccountInfo},
    entrypoint,
    entrypoint::ProgramResult,
    instruction::{AccountMeta, Instruction},
    program::invoke,
    program_error::ProgramError,
    pubkey::Pubkey,
};
use spl_token_swap::instruction::{swap, Swap};

#[derive(BorshSerialize, BorshDeserialize)]
struct SwapParams {
    amount_in: u64,
    minimum_amount_out: u64,
}

entrypoint!(process_instruction);
fn process_instruction(
    _program_id: &Pubkey,
    accounts: &[AccountInfo],
    instruction_data: &[u8],
) -> ProgramResult {
    let account_info_iter = &mut accounts.iter();
    let token_swap_program = next_account_info(account_info_iter)?;
    let swap_info = next_account_info(account_info_iter)?;
    let authority_info = next_account_info(account_info_iter)?;
    let user_transfer_authority_info = next_account_info(account_info_iter)?;
    let source_info = next_account_info(account_info_iter)?;
    let swap_source_info = next_account_info(account_info_iter)?;
    let swap_destination_info = next_account_info(account_info_iter)?;
    let destination_info = next_account_info(account_info_iter)?;
    let pool_mint_info = next_account_info(account_info_iter)?;
    let pool_fee_account_info = next_account_info(account_info_iter)?;
    let token_program_info = next_account_info(account_info_iter)?;

    let swap_params = SwapParams::try_from_slice(instruction_data)?;

    let swap_ix = swap(
        token_swap_program.key,
        &spl_token::id(),
        swap_info.key,
        authority_info.key,
        user_transfer_authority_info.key,
        source_info.key,
        swap_source_info.key,
        swap_destination_info.key,
        destination_info.key,
        pool_mint_info.key,
        pool_fee_account_info.key,
        None,
        Swap {
            amount_in: swap_params.amount_in,
            minimum_amount_out: swap_params.minimum_amount_out,
        },
    )?;
    invoke(
        &swap_ix,
        &[
            swap_info.clone(),
            authority_info.clone(),
            user_transfer_authority_info.clone(),
            source_info.clone(),
            swap_source_info.clone(),
            swap_destination_info.clone(),
            destination_info.clone(),
            pool_mint_info.clone(),
            pool_fee_account_info.clone(),
            token_program_info.clone(),
            token_swap_program.clone(),
        ],
    )?;
    Ok(())
}

pub fn inner_swap(
    program_id: &Pubkey,
    token_swap_program: &Pubkey,
    token_program_id: &Pubkey,
    swap_pubkey: &Pubkey,
    authority_pubkey: &Pubkey,
    user_transfer_authority_pubkey: &Pubkey,
    source_pubkey: &Pubkey,
    swap_source_pubkey: &Pubkey,
    swap_destination_pubkey: &Pubkey,
    destination_pubkey: &Pubkey,
    pool_mint_pubkey: &Pubkey,
    pool_fee_pubkey: &Pubkey,
    amount_in: u64,
    minimum_amount_out: u64,
) -> Result<Instruction, ProgramError> {
    let data = SwapParams {
        amount_in,
        minimum_amount_out,
    }
    .try_to_vec()?;

    let accounts = vec![
        AccountMeta::new_readonly(*token_swap_program, false),
        AccountMeta::new_readonly(*swap_pubkey, false),
        AccountMeta::new_readonly(*authority_pubkey, false),
        AccountMeta::new_readonly(*user_transfer_authority_pubkey, true),
        AccountMeta::new(*source_pubkey, false),
        AccountMeta::new(*swap_source_pubkey, false),
        AccountMeta::new(*swap_destination_pubkey, false),
        AccountMeta::new(*destination_pubkey, false),
        AccountMeta::new(*pool_mint_pubkey, false),
        AccountMeta::new(*pool_fee_pubkey, false),
        AccountMeta::new_readonly(*token_program_id, false),
    ];

    Ok(Instruction {
        program_id: *program_id,
        accounts,
        data,
    })
}
