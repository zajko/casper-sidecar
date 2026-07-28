use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use casper_binary_port::EvmSpeculativeExecutionResult;
use casper_json_rpc::{Error as RpcError, Params, ReservedErrorCode};
use casper_types::{BlockIdentifier, EvmConfig, EvmTransaction, TimeDiff, Timestamp, U256, evm};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    super::{Error as RpcServerError, NodeClient, RpcWithParams},
    config::read_evm_config,
    eth_u256::EthU256,
    projection::evm_hash_to_block_hash,
    types::{
        BlockNumberParam, BlockTag, DEFAULT_ETH_CALL_GAS_LIMIT, EthAddress, HexData,
        internal_error, invalid_params, parse_positional_params,
    },
};
use crate::{ClientError, rpcs::docs::DocExample};

static CALL_PARAMS_EXAMPLE: LazyLock<CallParams> = LazyLock::new(|| CallParams {
    call: CallObject {
        from: Some(EthAddress::from(evm::Address::ZERO)),
        to: None,
        data: Some(HexData::from(Vec::new())),
        input: None,
        value: None,
        gas: None,
        gas_price: None,
    },
    block: CallBlockParam::Number(BlockNumberParam::Tag(BlockTag::Latest)),
});

const DEFAULT_EVM_CALL_TTL: TimeDiff = TimeDiff::from_seconds(300);

/// Call object accepted by `eth_call` and `eth_estimateGas`.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CallObject {
    from: Option<EthAddress>,
    to: Option<EthAddress>,
    data: Option<HexData>,
    input: Option<HexData>,
    value: Option<EthU256>,
    gas: Option<EthU256>,
    gas_price: Option<EthU256>,
}

impl CallObject {
    fn from(&self) -> evm::Address {
        self.from
            .map(EthAddress::into_inner)
            .unwrap_or(evm::Address::ZERO)
    }

    fn to(&self) -> Option<evm::Address> {
        self.to.map(EthAddress::into_inner)
    }

    fn input(&self) -> Result<Vec<u8>, RpcError> {
        match (&self.data, &self.input) {
            (Some(data), Some(input)) if data != input => Err(invalid_params(
                "eth_call data and input fields must match when both are supplied",
            )),
            (Some(data), _) => Ok(data.clone().into_bytes()),
            (_, Some(input)) => Ok(input.clone().into_bytes()),
            (None, None) => Ok(Vec::new()),
        }
    }

    fn value(&self) -> U256 {
        self.value
            .as_ref()
            .map(|value| value.value())
            .unwrap_or_else(U256::zero)
    }

    fn gas_limit(&self) -> Result<u64, RpcError> {
        self.gas
            .map(EthU256::as_u64)
            .transpose()
            .map_err(invalid_params)
            .map(|maybe_gas| maybe_gas.unwrap_or(DEFAULT_ETH_CALL_GAS_LIMIT))
    }

    fn gas_price(&self, default_base_fee: u128) -> Result<u128, RpcError> {
        self.gas_price
            .map(eth_u256_to_u128)
            .transpose()
            .map(|maybe_gas_price| maybe_gas_price.unwrap_or(default_base_fee))
    }
}

/// Params for `eth_call`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CallParams {
    call: CallObject,
    #[serde(default)]
    block: CallBlockParam,
}

/// Block selector accepted by `eth_call`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
enum CallBlockParam {
    /// A block height or named tag.
    Number(BlockNumberParam),
    /// An EIP-1898 block hash selector.
    Hash(CallBlockHashParam),
}

impl Default for CallBlockParam {
    fn default() -> Self {
        CallBlockParam::Number(BlockNumberParam::default())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CallBlockHashParam {
    block_hash: evm::Hash,
    /// Accepted for EIP-1898 compatibility. Speculative execution only resolves complete blocks.
    #[serde(default)]
    require_canonical: bool,
}

impl CallParams {
    fn call(&self) -> &CallObject {
        &self.call
    }

    fn block_identifier(&self) -> Result<Option<BlockIdentifier>, RpcError> {
        match self.block {
            CallBlockParam::Number(BlockNumberParam::Tag(BlockTag::Pending)) => {
                Err(invalid_params("eth_call does not support pending state"))
            }
            CallBlockParam::Number(block) => block.identifier(),
            CallBlockParam::Hash(CallBlockHashParam {
                block_hash,
                require_canonical: _,
            }) => Ok(Some(BlockIdentifier::Hash(evm_hash_to_block_hash(
                block_hash,
            )))),
        }
    }
}

impl DocExample for CallParams {
    fn doc_example() -> &'static Self {
        &CALL_PARAMS_EXAMPLE
    }
}

#[derive(Deserialize)]
struct PositionalParams(CallObject, #[serde(default)] CallBlockParam);

impl From<PositionalParams> for CallParams {
    fn from(params: PositionalParams) -> Self {
        CallParams {
            call: params.0,
            block: params.1,
        }
    }
}

fn eth_u256_to_u128(value: EthU256) -> Result<u128, RpcError> {
    let value = value.value();
    if value > U256::from(u128::MAX) {
        return Err(invalid_params("quantity exceeds u128"));
    }
    let mut bytes = [0u8; 32];
    value.to_big_endian(&mut bytes);
    Ok(u128::from_be_bytes(
        bytes[16..]
            .try_into()
            .expect("slice should contain exactly sixteen bytes"),
    ))
}

pub(super) fn new_evm_call_transaction(
    call: &CallObject,
    evm_config: EvmConfig,
) -> Result<EvmTransaction, RpcError> {
    let from = call.from();
    Ok(EvmTransaction::new_unsigned_call(
        Timestamp::zero(),
        DEFAULT_EVM_CALL_TTL,
        evm_config.chain_id,
        from,
        call.to(),
        call.value(),
        call.input()?,
        call.gas_limit()?,
        call.gas_price(evm_config.base_fee_wei())?,
    ))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EthCallError {
    message: String,
    data: HexData,
    gas_used: EthU256,
}

pub(super) async fn execute_evm_call(
    node_client: &dyn NodeClient,
    call: &CallObject,
    block_identifier: Option<BlockIdentifier>,
) -> Result<EvmSpeculativeExecutionResult, RpcError> {
    let evm_config = read_evm_config(node_client).await?;
    let transaction = new_evm_call_transaction(call, evm_config)?;
    match node_client.evm_call(transaction, block_identifier).await {
        Ok(result) => Ok(result),
        Err(ClientError::NotFound) => {
            let available_block_range = node_client
                .read_available_block_range()
                .await
                .map_err(internal_error)?;
            Err(RpcServerError::NoBlockFound(block_identifier, available_block_range).into())
        }
        Err(ClientError::UnsupportedRequest | ClientError::MalformedCommand)
            if block_identifier.is_some() =>
        {
            Err(invalid_params(
                "eth_call is not supported at the requested block",
            ))
        }
        Err(error) => Err(internal_error(error)),
    }
}

pub(super) fn ensure_evm_call_succeeded(
    result: &EvmSpeculativeExecutionResult,
) -> Result<(), RpcError> {
    let receipt = result.evm_receipt();
    if receipt.status.is_success() {
        return Ok(());
    }
    Err(RpcError::new(
        ReservedErrorCode::InternalError,
        EthCallError {
            message: receipt
                .status
                .message()
                .unwrap_or("EVM call failed")
                .to_string(),
            data: HexData::from(result.evm_output()),
            gas_used: EthU256::from(receipt.gas_used),
        },
    ))
}

/// `eth_call`.
pub struct Call;

#[async_trait]
impl RpcWithParams for Call {
    const METHOD: &'static str = "eth_call";
    type RequestParams = CallParams;
    type ResponseResult = HexData;

    fn try_parse_params(maybe_params: Option<Params>) -> Result<Self::RequestParams, RpcError> {
        parse_positional_params::<PositionalParams>(maybe_params).map(Into::into)
    }

    async fn do_handle_request(
        node_client: Arc<dyn NodeClient>,
        params: CallParams,
    ) -> Result<HexData, RpcError> {
        let block_identifier = params.block_identifier()?;
        let result =
            execute_evm_call(node_client.as_ref(), params.call(), block_identifier).await?;
        ensure_evm_call_succeeded(&result)?;
        Ok(HexData::from(result.evm_output()))
    }
}

#[cfg(test)]
mod tests {
    use casper_binary_port::{
        BinaryResponse, Command, ErrorCode as BinaryPortErrorCode, InformationRequest,
    };
    use casper_types::{
        AvailableBlockRange, BlockHash, ChainspecRawBytes, Gas,
        bytesrepr::Bytes,
        evm::{Receipt, ReceiptStatus},
        execution::Effects,
    };
    use serde_json::json;

    use super::*;
    use crate::rpcs::test_utils::BinaryPortMock;

    const EVM_CHAIN_ID: u64 = 7;
    const EVM_BASE_FEE: u64 = 3;
    const EVM_WEI_PER_MOTE: u64 = 1_000_000_000;

    #[test]
    fn eth_call_accepts_numeric_block_height() {
        let params = Call::try_parse_params(Some(Params::Array(vec![
            json!({
                "to": format!("0x{}", "01".repeat(evm::ADDRESS_LENGTH)),
                "data": "0x95d89b41",
            }),
            json!("0xc"),
        ])))
        .expect("numeric block selector should parse");

        assert_eq!(
            params.block,
            CallBlockParam::Number(BlockNumberParam::Height(EthU256::from(12u64)))
        );
    }

    #[test]
    fn eth_call_accepts_block_hash_object() {
        let evm_hash = evm::Hash::new([7; evm::HASH_LENGTH]);
        let params = Call::try_parse_params(Some(Params::Array(vec![
            json!({}),
            json!({
                "blockHash": format!("0x{}", evm_hash.to_hex_string()),
                "requireCanonical": true,
            }),
        ])))
        .expect("block hash selector should parse");

        assert_eq!(
            params.block,
            CallBlockParam::Hash(CallBlockHashParam {
                block_hash: evm_hash,
                require_canonical: true,
            })
        );
        assert_eq!(
            params
                .block_identifier()
                .expect("block hash selector should be supported"),
            Some(BlockIdentifier::Hash(evm_hash_to_block_hash(evm_hash)))
        );
    }

    #[test]
    fn eth_call_defaults_to_latest_block() {
        let params = Call::try_parse_params(Some(Params::Array(vec![json!({})])))
            .expect("omitted block selector should default to latest");

        assert_eq!(
            params.block,
            CallBlockParam::Number(BlockNumberParam::Tag(BlockTag::Latest))
        );
    }

    #[test]
    fn eth_call_maps_supported_block_selectors() {
        let selectors = [
            (
                BlockNumberParam::Height(EthU256::from(12u64)),
                Some(BlockIdentifier::Height(12)),
            ),
            (
                BlockNumberParam::Tag(BlockTag::Earliest),
                Some(BlockIdentifier::Height(0)),
            ),
            (BlockNumberParam::Tag(BlockTag::Latest), None),
            (BlockNumberParam::Tag(BlockTag::Safe), None),
            (BlockNumberParam::Tag(BlockTag::Finalized), None),
        ];

        for (block, expected) in selectors {
            assert_eq!(
                CallParams {
                    call: CallObject::default(),
                    block: CallBlockParam::Number(block),
                }
                .block_identifier()
                .expect("block selector should be supported"),
                expected
            );
        }
    }

    #[test]
    fn eth_call_rejects_pending_block_selector() {
        let error = CallParams {
            call: CallObject::default(),
            block: CallBlockParam::Number(BlockNumberParam::Tag(BlockTag::Pending)),
        }
        .block_identifier()
        .expect_err("pending state should be rejected");

        assert_eq!(
            error,
            invalid_params("eth_call does not support pending state")
        );
    }

    #[tokio::test]
    async fn eth_call_sends_numeric_height_to_speculative_execution() {
        let client = BinaryPortMock::new();
        let chainspec_request = InformationRequest::ChainspecRawBytes
            .try_into()
            .expect("chainspec information request should convert");
        client
            .when_then(
                Command::Get(chainspec_request),
                BinaryResponse::from_value(chainspec()),
            )
            .await;

        let params = CallParams {
            call: CallObject::default(),
            block: CallBlockParam::Number(BlockNumberParam::Height(EthU256::from(12u64))),
        };
        let transaction =
            new_evm_call_transaction(&params.call, evm_config()).expect("transaction should build");
        client
            .when_then(
                Command::TrySpeculativeExec {
                    transaction: casper_types::Transaction::Evm(Box::new(transaction)),
                    block_identifier: Some(BlockIdentifier::Height(12)),
                },
                BinaryResponse::from_value(successful_result()),
            )
            .await;

        let result = Call::do_handle_request(Arc::new(client), params)
            .await
            .expect("historical call should succeed");

        assert_eq!(serde_json::to_value(result).unwrap(), json!("0x2a"));
    }

    #[tokio::test]
    async fn eth_call_sends_block_hash_to_speculative_execution() {
        let client = BinaryPortMock::new();
        let chainspec_request = InformationRequest::ChainspecRawBytes
            .try_into()
            .expect("chainspec information request should convert");
        client
            .when_then(
                Command::Get(chainspec_request),
                BinaryResponse::from_value(chainspec()),
            )
            .await;

        let block_hash = BlockHash::new([4; 32].into());
        let params = CallParams {
            call: CallObject::default(),
            block: CallBlockParam::Hash(CallBlockHashParam {
                block_hash: evm::Hash::new([4; evm::HASH_LENGTH]),
                require_canonical: false,
            }),
        };
        let transaction =
            new_evm_call_transaction(&params.call, evm_config()).expect("transaction should build");
        client
            .when_then(
                Command::TrySpeculativeExec {
                    transaction: casper_types::Transaction::Evm(Box::new(transaction)),
                    block_identifier: Some(BlockIdentifier::Hash(block_hash)),
                },
                BinaryResponse::from_value(successful_result()),
            )
            .await;

        let result = Call::do_handle_request(Arc::new(client), params)
            .await
            .expect("call at a block hash should succeed");

        assert_eq!(serde_json::to_value(result).unwrap(), json!("0x2a"));
    }

    #[tokio::test]
    async fn eth_call_reports_missing_historical_block() {
        let client = BinaryPortMock::new();
        let chainspec_request = InformationRequest::ChainspecRawBytes
            .try_into()
            .expect("chainspec information request should convert");
        client
            .when_then(
                Command::Get(chainspec_request),
                BinaryResponse::from_value(chainspec()),
            )
            .await;

        let params = CallParams {
            call: CallObject::default(),
            block: CallBlockParam::Number(BlockNumberParam::Height(EthU256::from(12u64))),
        };
        let transaction =
            new_evm_call_transaction(&params.call, evm_config()).expect("transaction should build");
        client
            .when_then(
                Command::TrySpeculativeExec {
                    transaction: casper_types::Transaction::Evm(Box::new(transaction)),
                    block_identifier: Some(BlockIdentifier::Height(12)),
                },
                BinaryResponse::new_error(BinaryPortErrorCode::NotFound),
            )
            .await;
        let available_range_request = InformationRequest::AvailableBlockRange
            .try_into()
            .expect("available range information request should convert");
        client
            .when_then(
                Command::Get(available_range_request),
                BinaryResponse::from_value(AvailableBlockRange::new(20, 30)),
            )
            .await;

        let error = Call::do_handle_request(Arc::new(client), params)
            .await
            .expect_err("missing historical block should fail");

        assert_eq!(error.code(), crate::rpcs::ErrorCode::NoSuchBlock as i64);
    }

    #[tokio::test]
    async fn eth_call_reports_unsupported_historical_execution() {
        for error_code in [
            BinaryPortErrorCode::UnsupportedRequest,
            BinaryPortErrorCode::MalformedCommand,
        ] {
            let client = BinaryPortMock::new();
            let chainspec_request = InformationRequest::ChainspecRawBytes
                .try_into()
                .expect("chainspec information request should convert");
            client
                .when_then(
                    Command::Get(chainspec_request),
                    BinaryResponse::from_value(chainspec()),
                )
                .await;

            let params = CallParams {
                call: CallObject::default(),
                block: CallBlockParam::Number(BlockNumberParam::Height(EthU256::from(12u64))),
            };
            let transaction = new_evm_call_transaction(&params.call, evm_config())
                .expect("transaction should build");
            client
                .when_then(
                    Command::TrySpeculativeExec {
                        transaction: casper_types::Transaction::Evm(Box::new(transaction)),
                        block_identifier: Some(BlockIdentifier::Height(12)),
                    },
                    BinaryResponse::new_error(error_code),
                )
                .await;

            let error = Call::do_handle_request(Arc::new(client), params)
                .await
                .expect_err("unsupported historical block should fail");

            assert_eq!(error.code(), ReservedErrorCode::InvalidParams as i64);
        }
    }

    #[test]
    fn eth_call_builds_unsigned_evm_transaction() {
        let from = evm::Address::new([1; evm::ADDRESS_LENGTH]);
        let to = evm::Address::new([2; evm::ADDRESS_LENGTH]);
        let input = vec![0xde, 0xad];

        let transaction = new_evm_call_transaction(
            &CallObject {
                from: Some(EthAddress::from(from)),
                to: Some(EthAddress::from(to)),
                data: Some(HexData::from(input.clone())),
                input: None,
                value: Some(EthU256::from(U256::from(1))),
                gas: Some(EthU256::from(1_000u64)),
                gas_price: None,
            },
            evm_config(),
        )
        .expect("transaction should build");

        assert!(transaction.is_unsigned_call());
        assert!(transaction.approval().is_none());
        assert_eq!(transaction.chain_id(), Some(EVM_CHAIN_ID));
        assert_eq!(transaction.from(), from);
        assert_eq!(
            transaction.initiator_addr(),
            casper_types::InitiatorAddr::Eoa(from)
        );
        assert_eq!(transaction.to(), Some(to));
        assert_eq!(transaction.value(), U256::from(1));
        assert_eq!(transaction.input(), input.as_slice());
        assert_eq!(transaction.gas_limit(), 1_000);
        assert_eq!(
            transaction.gas_price(),
            Some(u128::from(EVM_BASE_FEE) * u128::from(EVM_WEI_PER_MOTE))
        );
    }

    #[test]
    fn eth_call_uses_explicit_gas_price() {
        let gas_price = u128::from(EVM_BASE_FEE) + 1;

        let transaction = new_evm_call_transaction(
            &CallObject {
                gas_price: Some(EthU256::from(gas_price)),
                ..CallObject::default()
            },
            evm_config(),
        )
        .expect("transaction should build");

        assert_eq!(transaction.gas_price(), Some(gas_price));
    }

    #[test]
    fn eth_u256_to_u128_rejects_overflow() {
        let result = eth_u256_to_u128(EthU256::from(U256::from(u128::MAX) + U256::one()));

        assert!(matches!(
            result,
            Err(error) if error.code() == ReservedErrorCode::InvalidParams as i64
        ));
    }

    fn evm_config() -> EvmConfig {
        EvmConfig {
            enabled: true,
            chain_id: EVM_CHAIN_ID,
            base_fee: EVM_BASE_FEE,
            wei_per_mote: EVM_WEI_PER_MOTE,
            ..Default::default()
        }
    }

    fn chainspec() -> ChainspecRawBytes {
        ChainspecRawBytes::new(
            br#"
[evm]
enabled = true
chain_id = 7
spec = "prague"
block_gas_limit = 30000000
base_fee = 3
wei_per_mote = 1000000000
"#
            .to_vec()
            .into(),
            None,
            None,
        )
    }

    fn successful_result() -> EvmSpeculativeExecutionResult {
        EvmSpeculativeExecutionResult::new(
            BlockHash::new([3; 32].into()),
            Gas::new(30_000_000u64),
            Gas::new(21_000u64),
            Effects::new(),
            None,
            Receipt {
                status: ReceiptStatus::Success,
                gas_used: 21_000,
                effective_gas_price: evm_config().base_fee_wei(),
                contract_address: None,
                logs: Vec::new(),
            },
            Bytes::from(vec![0x2a]),
        )
    }
}
