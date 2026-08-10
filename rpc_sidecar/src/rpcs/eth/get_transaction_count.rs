use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use casper_json_rpc::{Error as RpcError, Params};
use casper_types::{EvmAddr, Key, StoredValue, evm};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    super::{Error as RpcServerError, NodeClient, RpcWithParams},
    eth_u256::EthU256,
    types::{
        BlockNumberParam, BlockTag, EthAddress, PendingPolicy, StateBlockParam, internal_error,
        parse_positional_params,
    },
};
use crate::rpcs::docs::DocExample;

static GET_TRANSACTION_COUNT_PARAMS_EXAMPLE: LazyLock<GetTransactionCountParams> =
    LazyLock::new(|| GetTransactionCountParams {
        address: EthAddress::from(evm::Address::ZERO),
        block: StateBlockParam::Number(BlockNumberParam::Tag(BlockTag::Latest)),
    });

/// Params for `eth_getTransactionCount`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetTransactionCountParams {
    address: EthAddress,
    #[serde(default)]
    block: StateBlockParam,
}

impl GetTransactionCountParams {
    fn address(&self) -> evm::Address {
        self.address.into_inner()
    }
}

impl DocExample for GetTransactionCountParams {
    fn doc_example() -> &'static Self {
        &GET_TRANSACTION_COUNT_PARAMS_EXAMPLE
    }
}

#[derive(Deserialize)]
struct PositionalParams(EthAddress, #[serde(default)] StateBlockParam);

impl From<PositionalParams> for GetTransactionCountParams {
    fn from(params: PositionalParams) -> Self {
        GetTransactionCountParams {
            address: params.0,
            block: params.1,
        }
    }
}

/// `eth_getTransactionCount`.
pub struct GetTransactionCount;

#[async_trait]
impl RpcWithParams for GetTransactionCount {
    const METHOD: &'static str = "eth_getTransactionCount";
    type RequestParams = GetTransactionCountParams;
    type ResponseResult = EthU256;

    fn try_parse_params(maybe_params: Option<Params>) -> Result<Self::RequestParams, RpcError> {
        parse_positional_params::<PositionalParams>(maybe_params).map(Into::into)
    }

    async fn do_handle_request(
        node_client: Arc<dyn NodeClient>,
        params: GetTransactionCountParams,
    ) -> Result<EthU256, RpcError> {
        let address = params.address();
        let state_identifier = params
            .block
            .resolve_state_identifier(node_client.as_ref(), PendingPolicy::Latest)
            .await?;
        let maybe_value = node_client
            .query_global_state(state_identifier, Key::Evm(EvmAddr::Nonce(address)), vec![])
            .await
            .map_err(|error| RpcServerError::NodeRequest("EVM nonce", error))?;
        let nonce = match maybe_value.map(|value| value.into_inner().0) {
            Some(StoredValue::CLValue(cl_value)) => cl_value
                .into_t::<u64>()
                .map_err(|error| internal_error(format!("invalid EVM nonce CLValue: {error}")))?,
            Some(other) => {
                return Err(internal_error(format!(
                    "expected EVM nonce under key, found {}",
                    other.type_name()
                )));
            }
            None => 0,
        };
        Ok(EthU256::from(nonce))
    }
}

#[cfg(test)]
mod tests {
    use casper_binary_port::{
        BinaryResponse, Command, ErrorCode as BinaryPortErrorCode, GetRequest,
        GlobalStateEntityQualifier, GlobalStateQueryResult, GlobalStateRequest, InformationRequest,
    };
    use casper_types::{
        Block, BlockIdentifier, GlobalStateIdentifier, TestBlockBuilder, testing::TestRng,
    };

    use super::*;
    use crate::rpcs::{eth::types::BlockHashParam, test_utils::BinaryPortMock};

    const BLOCK_HEIGHT: u64 = 63;

    #[tokio::test]
    async fn get_transaction_count_reads_evm_nonce_at_numeric_height() {
        let client = BinaryPortMock::new();
        let address = evm::Address::new([1; evm::ADDRESS_LENGTH]);
        let state_identifier = Some(GlobalStateIdentifier::BlockHeight(BLOCK_HEIGHT));
        let block = Block::V2(
            TestBlockBuilder::new()
                .height(BLOCK_HEIGHT)
                .build(&mut TestRng::new()),
        );
        client
            .add_block_header_req_res(
                block.clone_header(),
                InformationRequest::BlockHeader(Some(BlockIdentifier::Height(BLOCK_HEIGHT))),
            )
            .await;
        let request = GlobalStateRequest::new(
            state_identifier,
            GlobalStateEntityQualifier::Item {
                base_key: Key::Evm(EvmAddr::Nonce(address)),
                path: Vec::new(),
            },
        );
        client
            .when_then(
                Command::Get(GetRequest::State(Box::new(request))),
                BinaryResponse::from_option(Some(GlobalStateQueryResult::new(
                    StoredValue::CLValue(casper_types::CLValue::from_t(12u64).unwrap()),
                    Vec::new(),
                ))),
            )
            .await;

        let result = GetTransactionCount::do_handle_request(
            Arc::new(client),
            GetTransactionCountParams {
                address: EthAddress::from(address),
                block: BlockNumberParam::Height(BLOCK_HEIGHT.into()).into(),
            },
        )
        .await
        .expect("nonce lookup should succeed");

        assert_eq!(result, EthU256::from(12u64));
    }

    #[test]
    fn parses_metamask_numeric_block_height() {
        let address = evm::Address::new([1; evm::ADDRESS_LENGTH]);
        let params = Params::Array(vec![
            serde_json::json!(String::from(EthAddress::from(address))),
            serde_json::json!("0x3f"),
        ]);

        let parsed = GetTransactionCount::try_parse_params(Some(params))
            .expect("numeric block height should parse");

        assert_eq!(
            parsed,
            GetTransactionCountParams {
                address: address.into(),
                block: BlockNumberParam::Height(BLOCK_HEIGHT.into()).into(),
            }
        );
    }

    #[test]
    fn parses_eip_1898_hash_object_selector() {
        let address = evm::Address::new([1; evm::ADDRESS_LENGTH]);
        let block_hash = evm::Hash::new([0x2a; evm::HASH_LENGTH]);
        let params = Params::Array(vec![
            serde_json::json!(String::from(EthAddress::from(address))),
            serde_json::json!({
                "blockHash": block_hash,
                "requireCanonical": false,
            }),
        ]);

        let parsed = GetTransactionCount::try_parse_params(Some(params))
            .expect("EIP-1898 block hash object should parse");

        assert_eq!(
            parsed.block,
            StateBlockParam::HashObject(BlockHashParam {
                block_hash,
                require_canonical: false,
            })
        );
    }

    #[tokio::test]
    async fn pruned_historical_state_returns_no_such_state_root() {
        let client = Arc::new(BinaryPortMock::new());
        let address = evm::Address::new([2; evm::ADDRESS_LENGTH]);
        let block = Block::V2(
            TestBlockBuilder::new()
                .height(BLOCK_HEIGHT)
                .build(&mut TestRng::new()),
        );
        client
            .add_block_header_req_res(
                block.clone_header(),
                InformationRequest::BlockHeader(Some(BlockIdentifier::Height(BLOCK_HEIGHT))),
            )
            .await;
        let request = GlobalStateRequest::new(
            Some(GlobalStateIdentifier::BlockHeight(BLOCK_HEIGHT)),
            GlobalStateEntityQualifier::Item {
                base_key: Key::Evm(EvmAddr::Nonce(address)),
                path: Vec::new(),
            },
        );
        client
            .when_then(
                Command::Get(GetRequest::State(Box::new(request))),
                BinaryResponse::new_error(BinaryPortErrorCode::RootNotFound),
            )
            .await;

        let error = GetTransactionCount::do_handle_request(
            client.clone(),
            GetTransactionCountParams {
                address: address.into(),
                block: BlockNumberParam::Height(BLOCK_HEIGHT.into()).into(),
            },
        )
        .await
        .expect_err("pruned nonce state should fail");

        assert_eq!(error.code(), crate::rpcs::ErrorCode::NoSuchStateRoot as i64);
        client.verify_no_lingering().await;
    }
}
