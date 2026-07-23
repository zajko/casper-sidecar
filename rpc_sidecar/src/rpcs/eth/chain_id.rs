use std::sync::Arc;

use async_trait::async_trait;
use casper_json_rpc::Error as RpcError;

use super::{
    super::{NodeClient, RpcWithoutParams},
    config::read_evm_config,
    eth_u256::EthU256,
};

/// `eth_chainId`.
pub struct ChainId;

#[async_trait]
impl RpcWithoutParams for ChainId {
    const METHOD: &'static str = "eth_chainId";
    type ResponseResult = EthU256;

    async fn do_handle_request(node_client: Arc<dyn NodeClient>) -> Result<EthU256, RpcError> {
        Ok(EthU256::from(
            read_evm_config(node_client.as_ref()).await?.chain_id,
        ))
    }
}
