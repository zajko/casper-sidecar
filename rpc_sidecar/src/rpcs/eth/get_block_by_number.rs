use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use casper_json_rpc::{Error as RpcError, Params};
use casper_types::evm;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    super::{NodeClient, RpcWithParams},
    eth_u256::EthU256,
    projection::project_block,
    types::{
        BlockNumberParam, BlockTag, DEFAULT_ETH_CALL_GAS_LIMIT, EthAddress, HexData,
        invalid_params, parse_positional_params,
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
    number: EthU256,
    hash: evm::Hash,
    parent_hash: evm::Hash,
    parent_beacon_block_root: evm::Hash,
    nonce: Option<HexData>,
    mix_hash: evm::Hash,
    sha3_uncles: evm::Hash,
    logs_bloom: HexData,
    transactions_root: evm::Hash,
    state_root: evm::Hash,
    receipts_root: evm::Hash,
    miner: EthAddress,
    difficulty: EthU256,
    total_difficulty: EthU256,
    extra_data: HexData,
    size: EthU256,
    gas_limit: EthU256,
    gas_used: EthU256,
    timestamp: EthU256,
    transactions: Vec<evm::Hash>,
    uncles: Vec<evm::Hash>,
    base_fee_per_gas: EthU256,
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
        let Some(block) = project_block(node_client, params.identifier()?).await? else {
            return Ok(None);
        };
        Ok(Some(BlockResponse {
            number: block.number,
            hash: block.hash,
            parent_hash: block.parent_hash,
            parent_beacon_block_root: block.parent_beacon_block_root,
            nonce: Some(HexData::from(vec![0; 8])),
            mix_hash: evm::Hash::ZERO,
            sha3_uncles: evm::EMPTY_CODE_HASH,
            logs_bloom: block.logs_bloom,
            transactions_root: block.transactions_root,
            state_root: block.state_root,
            receipts_root: block.receipts_root,
            miner: block.miner,
            difficulty: EthU256::from(0u8),
            total_difficulty: EthU256::from(0u8),
            extra_data: HexData::from(Vec::new()),
            size: EthU256::from(0u8),
            gas_limit: EthU256::from(DEFAULT_ETH_CALL_GAS_LIMIT),
            gas_used: block.gas_used,
            timestamp: block.timestamp,
            transactions: block.transactions,
            uncles: Vec::new(),
            base_fee_per_gas: EthU256::from(0u8),
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use casper_binary_port::InformationRequest;
    use casper_json_rpc::ReservedErrorCode;
    use casper_types::{
        Block, BlockSignatures, BlockWithSignatures, TestBlockBuilder, testing::TestRng,
    };
    use serde_json::json;

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

    #[tokio::test]
    async fn reports_block_proposer_as_miner() {
        let rng = &mut TestRng::new();
        let block = Block::V2(TestBlockBuilder::new().build(rng));
        let expected_miner = EthAddress::from(evm::Address::from_block_proposer_public_key(
            block.proposer(),
        ));
        let node_client = Arc::new(BinaryPortMock::new());
        node_client
            .add_block_with_signatures(
                BlockWithSignatures::new(block, BlockSignatures::random(rng)),
                InformationRequest::BlockWithSignatures(None),
            )
            .await;

        let response = GetBlockByNumber::do_handle_request(
            node_client.clone(),
            GetBlockByNumberParams {
                block: BlockNumberParam::Tag(BlockTag::Latest),
                full_transactions: false,
            },
        )
        .await
        .expect("request should succeed")
        .expect("block should be returned");

        assert_eq!(response.miner, expected_miner);
        node_client.verify_no_lingering().await;
    }

    #[test]
    fn serializes_parent_beacon_block_root_as_parent_hash() {
        let parent_hash = evm::Hash::new([7; evm::HASH_LENGTH]);
        let response = BlockResponse {
            number: EthU256::from(1u8),
            hash: evm::Hash::new([8; evm::HASH_LENGTH]),
            parent_hash,
            parent_beacon_block_root: parent_hash,
            nonce: Some(HexData::from(vec![0; 8])),
            mix_hash: evm::Hash::ZERO,
            sha3_uncles: evm::EMPTY_CODE_HASH,
            logs_bloom: HexData::from(Vec::new()),
            transactions_root: evm::Hash::ZERO,
            state_root: evm::Hash::ZERO,
            receipts_root: evm::Hash::ZERO,
            miner: EthAddress::from(evm::Address::ZERO),
            difficulty: EthU256::from(0u8),
            total_difficulty: EthU256::from(0u8),
            extra_data: HexData::from(Vec::new()),
            size: EthU256::from(0u8),
            gas_limit: EthU256::from(DEFAULT_ETH_CALL_GAS_LIMIT),
            gas_used: EthU256::from(0u8),
            timestamp: EthU256::from(1u8),
            transactions: Vec::new(),
            uncles: Vec::new(),
            base_fee_per_gas: EthU256::from(0u8),
        };

        let value = serde_json::to_value(response).expect("response should serialize");

        assert_eq!(
            value["parentBeaconBlockRoot"],
            serde_json::to_value(parent_hash).expect("hash should serialize")
        );
        assert_eq!(value["parentBeaconBlockRoot"], value["parentHash"]);
        assert_ne!(value["parentBeaconBlockRoot"], json!(null));
    }
}
