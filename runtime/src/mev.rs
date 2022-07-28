mod utils;

use std::{fs, io::Write, path::PathBuf};

use crossbeam_channel::{unbounded, Sender};
use log::error;
use serde::{ser::SerializeStruct, Serialize};
use solana_sdk::{
    account::ReadableAccount, clock::Slot, hash::Hash, instruction::CompiledInstruction,
    pubkey::Pubkey, transaction::SanitizedTransaction,
};
use spl_token::solana_program::program_pack::Pack;
use spl_token_swap::{instruction::SwapInstruction, state::SwapVersion};

use crate::{accounts::LoadedTransaction, mev::utils::serialize_b58};

/// MevLog saves the `log_send_channel` channel, where it can be passed and
/// cloned in the `Bank` structure. We spawn a thread on the initialization of
/// the struct to listen and log data in `log_path`.
#[derive(Debug)]
pub struct MevLog {
    pub log_path: PathBuf,
    pub log_send_channel: Sender<MevOpportunity>,
}

#[derive(Debug, Clone)]
pub struct Mev {
    pub log_send_channel: Sender<MevOpportunity>,
    pub orca_program: Pubkey,
}

#[derive(Debug)]
struct Fees(spl_token_swap::curve::fees::Fees);

impl Serialize for Fees {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut state = serializer.serialize_struct("Fees", 3)?;
        state.serialize_field("host_fee_denominator", &self.0.host_fee_denominator)?;
        state.serialize_field("host_fee_numerator", &self.0.host_fee_numerator)?;
        state.serialize_field(
            "owner_trade_fee_denominator",
            &self.0.owner_trade_fee_denominator,
        )?;
        state.serialize_field(
            "owner_trade_fee_numerator",
            &self.0.owner_trade_fee_numerator,
        )?;
        state.serialize_field("trade_fee_denominator", &self.0.trade_fee_denominator)?;
        state.serialize_field("trade_fee_numerator", &self.0.trade_fee_numerator)?;
        state.end()
    }
}

#[derive(Debug, Serialize)]
pub struct MevOpportunity {
    /// Amount from the source token.
    amount_in_a: u64,
    /// Minimum output from the destination token.
    minimum_amount_out_b: u64,

    /// Source account.
    #[serde(serialize_with = "serialize_b58")]
    user_a_account: Pubkey,
    user_a_pre_balance: u64,

    /// Destination account.
    #[serde(serialize_with = "serialize_b58")]
    user_b_account: Pubkey,
    user_b_pre_balance: u64,

    /// Source address, owned by the pool.
    #[serde(serialize_with = "serialize_b58")]
    pool_a_account: Pubkey,
    pool_a_pre_balance: u64,

    /// Destination address, owned by the pool.
    #[serde(serialize_with = "serialize_b58")]
    pool_b_account: Pubkey,
    pool_b_pre_balance: u64,

    // Should be the same as the `Pubkey` from the SDK.
    #[serde(serialize_with = "serialize_b58")]
    a_token_mint: spl_token::solana_program::pubkey::Pubkey,
    #[serde(serialize_with = "serialize_b58")]
    b_token_mint: spl_token::solana_program::pubkey::Pubkey,

    /// Fees.
    fees: Fees,

    /// Transaction hash.
    #[serde(serialize_with = "serialize_b58")]
    transaction_hash: Hash,

    slot: Slot,
}

impl Mev {
    pub fn new(log_send_channel: Sender<MevOpportunity>, orca_program: Pubkey) -> Self {
        Mev {
            log_send_channel,
            orca_program,
        }
    }

    fn get_orca_msg_opportunity(
        compiled_ix: &CompiledInstruction,
        loaded_transaction: &mut LoadedTransaction,
        transaction_hash: Hash,
        slot: Slot,
    ) -> Option<MevOpportunity> {
        let maybe_swap_ix = SwapInstruction::unpack(&compiled_ix.data);
        if let Ok(SwapInstruction::Swap(swap)) = maybe_swap_ix {
            let pool_addr_idx = *compiled_ix.accounts.get(0)?;
            let user_a_addr_idx = *compiled_ix.accounts.get(3)?;
            let pool_a_addr_idx = *compiled_ix.accounts.get(4)?;
            let pool_b_addr_idx = *compiled_ix.accounts.get(5)?;
            let user_b_addr_idx = *compiled_ix.accounts.get(5)?;

            let (user_a_addr, user_a_account) =
                loaded_transaction.accounts.get(user_a_addr_idx as usize)?;
            let (user_b_addr, user_b_account) =
                loaded_transaction.accounts.get(user_b_addr_idx as usize)?;

            let (pool_a_addr, pool_a_account) =
                loaded_transaction.accounts.get(pool_a_addr_idx as usize)?;
            let (pool_b_addr, pool_b_account) =
                loaded_transaction.accounts.get(pool_b_addr_idx as usize)?;

            let user_a_account = spl_token::state::Account::unpack(user_a_account.data()).ok()?;
            let user_b_account = spl_token::state::Account::unpack(user_b_account.data()).ok()?;

            let pool_a_account = spl_token::state::Account::unpack(pool_a_account.data()).ok()?;
            let pool_b_account = spl_token::state::Account::unpack(pool_b_account.data()).ok()?;

            let (_pool_addr, pool_account) =
                loaded_transaction.accounts.get(pool_addr_idx as usize)?;
            let pool = SwapVersion::unpack(&pool_account.data()).ok()?;

            Some(MevOpportunity {
                amount_in_a: swap.amount_in,
                minimum_amount_out_b: swap.minimum_amount_out,
                user_a_account: *user_a_addr,
                user_a_pre_balance: user_a_account.amount,
                user_b_account: *user_b_addr,
                user_b_pre_balance: user_b_account.amount,
                pool_a_account: *pool_a_addr,
                pool_a_pre_balance: pool_a_account.amount,
                pool_b_account: *pool_b_addr,
                pool_b_pre_balance: pool_b_account.amount,
                a_token_mint: pool_a_account.mint,
                b_token_mint: pool_b_account.mint,
                fees: Fees(pool.fees().clone()),
                transaction_hash,
                slot,
            })
        } else {
            None
        }
    }

    pub fn get_mev_opportunities(
        &self,
        tx: &SanitizedTransaction,
        loaded_transaction: &mut LoadedTransaction,
        slot: Slot,
    ) -> Vec<MevOpportunity> {
        let mut msg_opportunities = Vec::new();
        for (addr, compiled_ix) in tx.message().program_instructions_iter() {
            if addr == &self.orca_program {
                msg_opportunities.extend(Mev::get_orca_msg_opportunity(
                    compiled_ix,
                    loaded_transaction,
                    *tx.message_hash(),
                    slot,
                ));
            }
        }
        msg_opportunities
    }

    pub fn execute_and_log_mev_opportunities(&self, mev_opportunities: Vec<MevOpportunity>) {
        for mev_opportunity in mev_opportunities {
            if let Err(err) = self.log_send_channel.send(mev_opportunity) {
                error!("[MEV] Could not log arbitrage, error: {}", err);
            }
        }

        // TODO: Return something once we exploit arbitrage opportunities.
    }
}

impl MevLog {
    pub fn new(log_path: &PathBuf) -> Self {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(true)
            .open(log_path)
            .expect("Failed while creating/opening MEV log file");
        let (log_send_channel, log_receiver) = unbounded();

        std::thread::spawn(move || loop {
            match log_receiver.recv() {
                Ok(msg) => writeln!(
                    file,
                    "{}",
                    serde_json::to_string(&msg).expect("Constructed by us, should never fail")
                )
                .expect("[MEV] Could not write to file"),
                Err(err) => error!("[MEV] Could not log arbitrage on file, error: {}", err),
            }
        });

        MevLog {
            log_path: log_path.clone(),
            log_send_channel,
        }
    }
}

#[test]
fn test_serialization() {
    let opportunity = MevOpportunity {
        amount_in_a: 1,
        minimum_amount_out_b: 1,
        user_a_account: Pubkey::new_unique(),
        user_a_pre_balance: 1,
        user_b_account: Pubkey::new_unique(),
        user_b_pre_balance: 1,
        pool_a_account: Pubkey::new_unique(),
        pool_a_pre_balance: 1,
        pool_b_account: Pubkey::new_unique(),
        pool_b_pre_balance: 1,
        a_token_mint: spl_token::solana_program::pubkey::Pubkey::new_from_array(
            Pubkey::new_unique().to_bytes(),
        ),
        b_token_mint: spl_token::solana_program::pubkey::Pubkey::new_from_array(
            Pubkey::new_unique().to_bytes(),
        ),
        fees: Fees(spl_token_swap::curve::fees::Fees {
            trade_fee_numerator: 1,
            trade_fee_denominator: 10,
            owner_trade_fee_numerator: 1,
            owner_trade_fee_denominator: 10,
            owner_withdraw_fee_numerator: 1,
            owner_withdraw_fee_denominator: 10,
            host_fee_numerator: 1,
            host_fee_denominator: 10,
        }),
        transaction_hash: Hash::new_unique(),
        slot: 1,
    };

    let expected_result_str = "\
        {\
            'amount_in_a':1,\
            'minimum_amount_out_b':1,\
            'user_a_account':'4uQeVj5tqViQh7yWWGStvkEG1Zmhx6uasJtWCJziofM',\
            'user_a_pre_balance':1,\
            'user_b_account':'8opHzTAnfzRpPEx21XtnrVTX28YQuCpAjcn1PczScKh',\
            'user_b_pre_balance':1,\
            'pool_a_account':'CiDwVBFgWV9E5MvXWoLgnEgn2hK7rJikbvfWavzAQz3',\
            'pool_a_pre_balance':1,\
            'pool_b_account':'GcdayuLaLyrdmUu324nahyv33G5poQdLUEZ1nEytDeP',\
            'pool_b_pre_balance':1,\
            'a_token_mint':'LX3EUdRUBUa3TbsYXLEUdj9J3prXkWXvLYSWyYyc2Jj',\
            'b_token_mint':'QRSsyMWN1yHT9ir42bgNZUNZ4PdEhcSWCrL2AryKpy5',\
            'fees':{\
            'host_fee_denominator':10,\
            'host_fee_numerator':1,\
            'owner_trade_fee_denominator':10,\
            'owner_trade_fee_numerator':1,\
            'trade_fee_denominator':10,\
            'trade_fee_numerator':1\
            },\
            'transaction_hash':'4uQeVj5tqViQh7yWWGStvkEG1Zmhx6uasJtWCJziofM',\
            'slot':1\
        }"
    .replace("'", "\"");
    let serialized_json = serde_json::to_string(&opportunity).expect("Serialization failed");
    assert_eq!(serialized_json, expected_result_str);
}
