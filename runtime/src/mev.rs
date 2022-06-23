use std::fs::{self, File};
use std::io::Write;

use solana_sdk::transaction::SanitizedTransaction;

use crate::accounts::LoadedTransaction;

#[derive(Debug)]
pub struct MEV {
    pub log_path: &'static str,
    file: File,
}

impl MEV {
    pub fn new(log_path: &'static str) -> Self {
        let file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(true)
            .open(log_path)
            .expect("Failed while creating/opening MEV log file");
        MEV { log_path, file }
    }
    pub fn get_mev_transaction(
        &mut self,
        tx: &SanitizedTransaction,
        loaded_transaction: &mut LoadedTransaction,
    ) -> Option<(SanitizedTransaction, LoadedTransaction)> {
        writeln!(&mut self.file, "Observed MEV opportunity :-)").unwrap();
        // Change to return something once we exploit arbitrage opportunities.
        None
    }
}
