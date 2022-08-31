use std::{fs::read_to_string, path::PathBuf, str::FromStr};

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
    pub orca_accounts: AllOrcaPoolAddresses,
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

pub fn get_mev_config_file(config_path: &PathBuf) -> MevConfig {
    let config_str = read_to_string(config_path).expect("Could not open config path.");
    toml::from_str(&config_str).expect("Could not deserialize MEV config file.")
}
