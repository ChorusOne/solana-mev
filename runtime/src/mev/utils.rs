use std::{fs::File, io::BufReader, path::PathBuf, str::FromStr};

use serde::{Deserialize, Deserializer, Serializer};
use solana_sdk::pubkey::Pubkey;

use super::OrcaPoolAddresses;

#[derive(Debug, Deserialize)]
pub struct AllOrcaPoolAddresses(pub Vec<OrcaPoolAddresses>);

#[derive(Deserialize)]
pub struct MevConfig {
    pub log_path: PathBuf,
    #[serde(deserialize_with = "deserialize_b58")]
    pub orca_program_id: Pubkey,
    pub pool: String,
    pub accounts: AllOrcaPoolAddresses,
}

/// Function to use when serializing a public key, to print it using base58.
pub fn serialize_b58<S: Serializer, T: ToString>(x: &T, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&x.to_string())
}

/// Function to use when deserializing a public key.
pub fn deserialize_b58<'de, D>(deserializer: D) -> Result<Pubkey, D::Error>
where
    D: Deserializer<'de>,
{
    let buf = String::deserialize(deserializer)?;
    Pubkey::from_str(&buf).map_err(serde::de::Error::custom)
}

pub fn get_mev_config_file(config_path: &PathBuf) -> Result<MevConfig, serde_json::Error> {
    let file = File::open(config_path).expect("Could not open config path.");
    let reader = BufReader::new(file);
    serde_json::from_reader(reader)
}
