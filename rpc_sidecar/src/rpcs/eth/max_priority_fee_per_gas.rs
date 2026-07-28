use std::sync::Arc;

use async_trait::async_trait;
use casper_json_rpc::Error as RpcError;

use super::{
    super::{NodeClient, RpcWithoutParams},
    eth_u256::EthU256,
};

/// `eth_maxPriorityFeePerGas`.
pub struct MaxPriorityFeePerGas;

#[async_trait]
impl RpcWithoutParams for MaxPriorityFeePerGas {
    const METHOD: &'static str = "eth_maxPriorityFeePerGas";
    type ResponseResult = EthU256;

    async fn do_handle_request(_node_client: Arc<dyn NodeClient>) -> Result<EthU256, RpcError> {
        // Casper does not prioritize EVM transactions by proposer tips.
        Ok(EthU256::ZERO)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpcs::test_utils::BinaryPortMock;

    #[tokio::test]
    async fn returns_zero_priority_fee() {
        let result = MaxPriorityFeePerGas::do_handle_request(Arc::new(BinaryPortMock::new()))
            .await
            .expect("priority fee lookup should succeed");

        assert_eq!(result, EthU256::ZERO);
        assert_eq!(
            serde_json::to_value(result).unwrap(),
            serde_json::json!("0x0")
        );
    }
}
