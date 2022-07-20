use std::fs::{self};
use std::io::Write;
use std::path::PathBuf;

use crossbeam_channel::{unbounded, Sender};
use log::error;
use solana_sdk::instruction::CompiledInstruction;
use solana_sdk::{account::ReadableAccount, pubkey::Pubkey, transaction::SanitizedTransaction};
use spl_token::solana_program::program_pack::Pack;
use spl_token_swap::{instruction::SwapInstruction, state::SwapVersion};

use crate::accounts::LoadedTransaction;

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

pub struct MevOpportunity {
    amount_in: u64,
    minimum_amount_out: u64,
    token_swap_source: Pubkey,
    token_swap_source_amount: u64,
    token_swap_destination: Pubkey,
    token_swap_destination_amount: u64,
    fees: spl_token_swap::curve::fees::Fees,
}

impl std::fmt::Display for MevOpportunity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "Orca, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}",
            self.amount_in,
            self.minimum_amount_out,
            self.token_swap_source,
            self.token_swap_source_amount,
            self.token_swap_destination,
            self.token_swap_destination_amount,
            self.fees.trade_fee_numerator,
            self.fees.trade_fee_denominator,
            self.fees.owner_trade_fee_numerator,
            self.fees.owner_trade_fee_denominator,
            self.fees.host_fee_numerator,
            self.fees.host_fee_denominator,
        )
    }
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
    ) -> Option<MevOpportunity> {
        let swap_ix = SwapInstruction::unpack(&compiled_ix.data);
        if let Ok(SwapInstruction::Swap(swap)) = swap_ix {
            let swap_addr_idx = *compiled_ix.accounts.get(0)?;
            let swap_src_addr_idx = *compiled_ix.accounts.get(4)?;
            let swap_dst_addr_idx = *compiled_ix.accounts.get(5)?;

            let (swap_src_addr, token_swap_src_account) = loaded_transaction
                .accounts
                .get(swap_src_addr_idx as usize)?;
            let (swap_dst_addr, token_swap_dst_account) = loaded_transaction
                .accounts
                .get(swap_dst_addr_idx as usize)?;

            let token_swap_src_account =
                spl_token::state::Account::unpack(token_swap_src_account.data()).ok()?;
            let token_swap_dst_account =
                spl_token::state::Account::unpack(token_swap_dst_account.data()).ok()?;

            let (_token_swap_addr, token_swap_account) =
                loaded_transaction.accounts.get(swap_addr_idx as usize)?;
            let token_swap = SwapVersion::unpack(&token_swap_account.data()).ok()?;

            Some(MevOpportunity {
                amount_in: swap.amount_in,
                minimum_amount_out: swap.minimum_amount_out,
                token_swap_source: *swap_src_addr,
                token_swap_source_amount: token_swap_src_account.amount,
                token_swap_destination: *swap_dst_addr,
                token_swap_destination_amount: token_swap_dst_account.amount,
                fees: token_swap.fees().clone(),
            })
        } else {
            None
        }
    }

    pub fn get_mev_opportunities(
        &self,
        tx: &SanitizedTransaction,
        loaded_transaction: &mut LoadedTransaction,
    ) -> Vec<MevOpportunity> {
        let mut msg_opportunities = Vec::new();
        for (addr, compiled_ix) in tx.message().program_instructions_iter() {
            if addr == &self.orca_program {
                if let Some(mev_opportunity) =
                    Mev::get_orca_msg_opportunity(compiled_ix, loaded_transaction)
                {
                    msg_opportunities.push(mev_opportunity);
                }
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
                Ok(msg) => writeln!(file, "{}", msg).expect("[MEV] Could not write to file"),
                Err(err) => error!("[MEV] Could not log arbitrage on file, error: {}", err),
            }
        });

        MevLog {
            log_path: log_path.clone(),
            log_send_channel,
        }
    }
}
