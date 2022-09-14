use std::{fs::read_to_string, path::PathBuf, str::FromStr};

use serde::{Deserialize, Deserializer, Serializer};
use solana_sdk::pubkey::Pubkey;

use super::OrcaPoolAddresses;

#[derive(Debug, PartialEq, Deserialize)]
pub struct AllOrcaPoolAddresses(pub Vec<OrcaPoolAddresses>);

#[derive(Debug, PartialEq, Deserialize)]
pub struct MevConfig {
    pub log_path: PathBuf,
    #[serde(deserialize_with = "deserialize_b58")]
    pub orca_program_id: Pubkey,
    #[serde(rename(deserialize = "orca_account"))]
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

#[test]
fn test_deserialization() {
    let sample_config: MevConfig = toml::from_str(
        r#"
    log_path = '/tmp/mev.log'
    orca_program_id = '9W959DqEETiGZocYWCQPaJ6sBmUzgfxXfqGeTEdp3aQP'
    [[orca_account]]
        _id = 'USDC/USDT[stable]'
        address = 'FX5UWkujjpU4yKB4yvKVEzG2Z8r2PLmLpyVmv12yqAUQ'
        pool_a_account = 'EjUNm7Lzp6X8898JiCU28SbfQBfsYoWaViXUhCgizv82'
        pool_b_account = 'C1ZrV56rf1wbDzcnHY6FpNaVmzT5D8WtyEKS1FAGrboe'

    [[orca_account]]
        _id = 'SOL/USDC[aquafarm]'
        address = 'EGZ7tiLeH62TPV1gL8WwbXGzEPa9zmcpVnnkPKKnrE2U'
        pool_a_account = 'ANP74VNsHwSrq9uUSjiSNyNWvf6ZPrKTmE4gHoNd13Lg'
        pool_b_account = '75HgnSvXbWKZBpZHveX68ZzAhDqMzNDS29X6BGLtxMo1'
    "#,
    )
    .expect("Failed to deserialize");

    let expected_mev_config = MevConfig {
        log_path: PathBuf::from_str("/tmp/mev.log").unwrap(),
        orca_program_id: Pubkey::from_str("9W959DqEETiGZocYWCQPaJ6sBmUzgfxXfqGeTEdp3aQP").unwrap(),
        orca_accounts: AllOrcaPoolAddresses(vec![
            OrcaPoolAddresses {
                address: Pubkey::from_str("FX5UWkujjpU4yKB4yvKVEzG2Z8r2PLmLpyVmv12yqAUQ").unwrap(),
                pool_a_account: Pubkey::from_str("EjUNm7Lzp6X8898JiCU28SbfQBfsYoWaViXUhCgizv82")
                    .unwrap(),
                pool_b_account: Pubkey::from_str("C1ZrV56rf1wbDzcnHY6FpNaVmzT5D8WtyEKS1FAGrboe")
                    .unwrap(),
            },
            OrcaPoolAddresses {
                address: Pubkey::from_str("EGZ7tiLeH62TPV1gL8WwbXGzEPa9zmcpVnnkPKKnrE2U").unwrap(),
                pool_a_account: Pubkey::from_str("ANP74VNsHwSrq9uUSjiSNyNWvf6ZPrKTmE4gHoNd13Lg")
                    .unwrap(),
                pool_b_account: Pubkey::from_str("75HgnSvXbWKZBpZHveX68ZzAhDqMzNDS29X6BGLtxMo1")
                    .unwrap(),
            },
        ]),
    };
    assert_eq!(sample_config, expected_mev_config);
}