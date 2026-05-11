use std::sync::Arc;

use async_trait::async_trait;
use casper_json_rpc::Error as RpcError;
use casper_types::evm;
use serde::Deserialize;

use super::{
    super::{NodeClient, RpcWithoutParams},
    types::internal_error,
};

/// `eth_chainId`.
pub struct ChainId;

#[derive(Deserialize)]
struct ChainspecEvmConfig {
    evm: evm::EvmConfig,
}

#[async_trait]
impl RpcWithoutParams for ChainId {
    const METHOD: &'static str = "eth_chainId";
    type ResponseResult = evm::EthU256;

    async fn do_handle_request(node_client: Arc<dyn NodeClient>) -> Result<evm::EthU256, RpcError> {
        let chainspec = node_client
            .read_chainspec_bytes()
            .await
            .map_err(internal_error)?;
        let chainspec = std::str::from_utf8(chainspec.chainspec_bytes())
            .map_err(|error| internal_error(format!("invalid chainspec bytes: {error}")))?;
        let chainspec = toml::from_str::<ChainspecEvmConfig>(chainspec)
            .map_err(|error| internal_error(format!("invalid chainspec toml: {error}")))?;
        Ok(evm::EthU256::from(chainspec.evm.chain_id))
    }
}
