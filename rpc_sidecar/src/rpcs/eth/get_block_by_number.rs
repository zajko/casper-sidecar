use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use casper_json_rpc::{Error as RpcError, Params};
use casper_types::evm;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    super::{NodeClient, RpcWithParams},
    types::{
        BlockNumberParam, BlockTag, DEFAULT_ETH_CALL_GAS_LIMIT, ETH_LOG_BLOOM_LENGTH, EthAddress,
        HexData, block_hash_to_evm_hash, digest_to_evm_hash, internal_error, invalid_params,
        parse_positional_params, transaction_hash_to_evm_hash,
    },
};
use crate::rpcs::docs::DocExample;

static GET_BLOCK_BY_NUMBER_PARAMS_EXAMPLE: LazyLock<GetBlockByNumberParams> =
    LazyLock::new(|| GetBlockByNumberParams {
        block: BlockNumberParam::Tag(BlockTag::Latest),
        full_transactions: false,
    });
static BLOCK_RESPONSE_EXAMPLE: LazyLock<Option<BlockResponse>> = LazyLock::new(|| None);

/// Params for `eth_getBlockByNumber`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetBlockByNumberParams {
    block: BlockNumberParam,
    full_transactions: bool,
}

impl GetBlockByNumberParams {
    fn identifier(&self) -> Result<Option<casper_types::BlockIdentifier>, RpcError> {
        self.block.identifier()
    }

    fn full_transactions(&self) -> bool {
        self.full_transactions
    }
}

impl DocExample for GetBlockByNumberParams {
    fn doc_example() -> &'static Self {
        &GET_BLOCK_BY_NUMBER_PARAMS_EXAMPLE
    }
}

#[derive(Deserialize)]
struct PositionalParams(BlockNumberParam, #[serde(default)] bool);

impl From<PositionalParams> for GetBlockByNumberParams {
    fn from(params: PositionalParams) -> Self {
        GetBlockByNumberParams {
            block: params.0,
            full_transactions: params.1,
        }
    }
}

/// Ethereum block response returned by `eth_getBlockByNumber`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BlockResponse {
    number: evm::EthU256,
    hash: evm::Hash,
    parent_hash: evm::Hash,
    nonce: Option<HexData>,
    mix_hash: evm::Hash,
    sha3_uncles: evm::Hash,
    logs_bloom: HexData,
    transactions_root: evm::Hash,
    state_root: evm::Hash,
    receipts_root: evm::Hash,
    miner: EthAddress,
    difficulty: evm::EthU256,
    total_difficulty: evm::EthU256,
    extra_data: HexData,
    size: evm::EthU256,
    gas_limit: evm::EthU256,
    gas_used: evm::EthU256,
    timestamp: evm::EthU256,
    transactions: Vec<evm::Hash>,
    uncles: Vec<evm::Hash>,
    base_fee_per_gas: evm::EthU256,
}

impl DocExample for Option<BlockResponse> {
    fn doc_example() -> &'static Self {
        &BLOCK_RESPONSE_EXAMPLE
    }
}

/// `eth_getBlockByNumber`.
pub struct GetBlockByNumber;

#[async_trait]
impl RpcWithParams for GetBlockByNumber {
    const METHOD: &'static str = "eth_getBlockByNumber";
    type RequestParams = GetBlockByNumberParams;
    type ResponseResult = Option<BlockResponse>;

    fn try_parse_params(maybe_params: Option<Params>) -> Result<Self::RequestParams, RpcError> {
        parse_positional_params::<PositionalParams>(maybe_params).map(Into::into)
    }

    async fn do_handle_request(
        node_client: Arc<dyn NodeClient>,
        params: GetBlockByNumberParams,
    ) -> Result<Option<BlockResponse>, RpcError> {
        if params.full_transactions() {
            // TODO: Return full Ethereum transaction objects once the sidecar has
            // an Ethereum-compatible transaction response type.
            return Err(invalid_params(
                "full transaction objects are not supported yet",
            ));
        }
        let Some(block) = node_client
            .read_block_with_signatures(params.identifier()?)
            .await
            .map_err(internal_error)?
        else {
            return Ok(None);
        };
        let block = block.block();
        let transactions = block
            .all_transaction_hashes()
            .filter_map(transaction_hash_to_evm_hash)
            .collect::<Vec<_>>();
        Ok(Some(BlockResponse {
            number: evm::EthU256::from(block.height()),
            hash: block_hash_to_evm_hash(block.hash()),
            parent_hash: block_hash_to_evm_hash(block.parent_hash()),
            nonce: Some(HexData::from(vec![0; 8])),
            mix_hash: evm::Hash::ZERO,
            sha3_uncles: evm::EMPTY_CODE_HASH,
            logs_bloom: HexData::from(vec![0; ETH_LOG_BLOOM_LENGTH]),
            transactions_root: digest_to_evm_hash(block.body_hash()),
            state_root: digest_to_evm_hash(block.state_root_hash()),
            receipts_root: evm::Hash::ZERO,
            miner: EthAddress::from(evm::Address::ZERO),
            difficulty: evm::EthU256::from(0u8),
            total_difficulty: evm::EthU256::from(0u8),
            extra_data: HexData::from(Vec::new()),
            size: evm::EthU256::from(0u8),
            gas_limit: evm::EthU256::from(DEFAULT_ETH_CALL_GAS_LIMIT),
            gas_used: evm::EthU256::from(0u8),
            timestamp: evm::EthU256::from(block.timestamp().millis() / 1_000),
            transactions,
            uncles: Vec::new(),
            base_fee_per_gas: evm::EthU256::from(0u8),
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use casper_json_rpc::ReservedErrorCode;

    use super::*;
    use crate::rpcs::test_utils::BinaryPortMock;

    #[tokio::test]
    async fn rejects_full_transaction_blocks_until_supported() {
        let err = GetBlockByNumber::do_handle_request(
            Arc::new(BinaryPortMock::new()),
            GetBlockByNumberParams {
                block: BlockNumberParam::Tag(BlockTag::Latest),
                full_transactions: true,
            },
        )
        .await
        .expect_err("full transaction objects are not supported yet");

        assert_eq!(
            err,
            RpcError::new(
                ReservedErrorCode::InvalidParams,
                "full transaction objects are not supported yet"
            )
        );
    }
}
