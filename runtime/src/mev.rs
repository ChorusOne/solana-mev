use std::fs::{self};
use std::io::Write;
use std::path::PathBuf;

use crossbeam_channel::{unbounded, Sender};
use log::error;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::transaction::SanitizedTransaction;

use crate::accounts::LoadedTransaction;

/// MevLog saves the `log_send_channel` channel, where it can be passed and
/// cloned in the `Bank` structure. We spawn a thread on the initialization of
/// the struct to listen and log data in `log_path`.
#[derive(Debug)]
pub struct MevLog {
    pub log_path: PathBuf,
    pub log_send_channel: Sender<String>,
}

#[derive(Debug, Clone)]
pub struct Mev {
    pub log_send_channel: Sender<String>,
    pub orca_program: Pubkey,
}

impl Mev {
    pub fn new(log_send_channel: Sender<String>, orca_program: Pubkey) -> Self {
        Mev {
            log_send_channel,
            orca_program,
        }
    }

    pub fn get_mev_transaction(
        &self,
        tx: &SanitizedTransaction,
        loaded_transaction: &mut LoadedTransaction,
    ) -> Option<(SanitizedTransaction, LoadedTransaction)> {
        if let Err(err) = self
            .log_send_channel
            .send("Observed MEV opportunity :-)".to_owned())
        {
            error!("[MEV] Could not log arbitrage, error: {}", err);
        }
        // Change to return something once we exploit arbitrage opportunities.
        None
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
