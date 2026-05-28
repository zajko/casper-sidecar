use std::sync::Arc;

use async_trait::async_trait;
use casper_json_rpc::Error as RpcError;

use super::{
    super::{NodeClient, RpcWithoutParams},
    eth_u256::EthU256,
    types::internal_error,
};

/// `eth_blockNumber`.
pub struct BlockNumber;

#[async_trait]
impl RpcWithoutParams for BlockNumber {
    const METHOD: &'static str = "eth_blockNumber";
    type ResponseResult = EthU256;

    async fn do_handle_request(node_client: Arc<dyn NodeClient>) -> Result<EthU256, RpcError> {
        let block_header = node_client
            .read_block_header(None)
            .await
            .map_err(internal_error)?
            .ok_or_else(|| internal_error("latest block header was not found"))?;
        Ok(EthU256::from(block_header.height()))
    }
}
