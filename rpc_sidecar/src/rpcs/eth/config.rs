use casper_json_rpc::Error as RpcError;
use casper_types::EvmConfig;
use serde::Deserialize;

use super::{super::NodeClient, types::internal_error};

#[derive(Deserialize)]
struct ChainspecEvmConfig {
    evm: EvmConfig,
}

pub(super) async fn read_evm_config(node_client: &dyn NodeClient) -> Result<EvmConfig, RpcError> {
    let chainspec = node_client
        .read_chainspec_bytes()
        .await
        .map_err(internal_error)?;
    let chainspec = std::str::from_utf8(chainspec.chainspec_bytes())
        .map_err(|error| internal_error(format!("invalid chainspec bytes: {error}")))?;
    let chainspec = toml::from_str::<ChainspecEvmConfig>(chainspec)
        .map_err(|error| internal_error(format!("invalid chainspec toml: {error}")))?;
    Ok(chainspec.evm)
}
