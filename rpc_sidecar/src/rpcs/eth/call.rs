use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use casper_binary_port::EvmCallRequest;
use casper_json_rpc::{Error as RpcError, Params, ReservedErrorCode};
use casper_types::{U256, bytesrepr::Bytes, evm};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    super::{NodeClient, RpcWithParams},
    types::{
        BlockTag, DEFAULT_ETH_CALL_GAS_LIMIT, EthAddress, HexData, internal_error, invalid_params,
        parse_positional_params,
    },
};
use crate::rpcs::docs::DocExample;

static CALL_PARAMS_EXAMPLE: LazyLock<CallParams> = LazyLock::new(|| CallParams {
    call: CallObject {
        from: Some(EthAddress::from(evm::Address::ZERO)),
        to: None,
        data: Some(HexData::from(Vec::new())),
        input: None,
        value: None,
        gas: None,
    },
    block: BlockTag::Latest,
});

/// Call object accepted by `eth_call`.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CallObject {
    from: Option<EthAddress>,
    to: Option<EthAddress>,
    data: Option<HexData>,
    input: Option<HexData>,
    value: Option<evm::EthU256>,
    gas: Option<evm::EthU256>,
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
            .map(evm::EthU256::as_u64)
            .transpose()
            .map_err(invalid_params)
            .map(|maybe_gas| maybe_gas.unwrap_or(DEFAULT_ETH_CALL_GAS_LIMIT))
    }
}

/// Params for `eth_call`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CallParams {
    call: CallObject,
    #[serde(default)]
    block: BlockTag,
}

impl CallParams {
    fn call(&self) -> &CallObject {
        &self.call
    }
}

impl DocExample for CallParams {
    fn doc_example() -> &'static Self {
        &CALL_PARAMS_EXAMPLE
    }
}

#[derive(Deserialize)]
struct PositionalParams(CallObject, #[serde(default)] BlockTag);

impl From<PositionalParams> for CallParams {
    fn from(params: PositionalParams) -> Self {
        CallParams {
            call: params.0,
            block: params.1,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EthCallError {
    message: String,
    data: HexData,
    gas_used: evm::EthU256,
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
        let call = params.call();
        let result = node_client
            .evm_call(EvmCallRequest::new(
                call.from(),
                call.to(),
                call.value(),
                Bytes::from(call.input()?),
                call.gas_limit()?,
            ))
            .await
            .map_err(internal_error)?;
        if result.status().is_success() {
            Ok(HexData::from(result.output()))
        } else {
            Err(RpcError::new(
                ReservedErrorCode::InternalError,
                EthCallError {
                    message: result
                        .status()
                        .message()
                        .unwrap_or("EVM call failed")
                        .to_string(),
                    data: HexData::from(result.output()),
                    gas_used: evm::EthU256::from(result.gas_used()),
                },
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use casper_binary_port::{
        BinaryResponse, Command, EvmCallResult, SimulationRequest, SimulationResult,
        SpeculativeExecutionResult,
    };

    use super::*;
    use crate::rpcs::test_utils::BinaryPortMock;

    #[tokio::test]
    async fn eth_call_success_uses_binary_port_simulate() {
        let client = BinaryPortMock::new();
        let from = evm::Address::new([1; evm::ADDRESS_LENGTH]);
        let to = evm::Address::new([2; evm::ADDRESS_LENGTH]);
        let input = vec![0xde, 0xad];
        let output = vec![0x12, 0x34];
        client
            .when_then(
                Command::Simulate {
                    request: SimulationRequest::EvmCall(EvmCallRequest::new(
                        from,
                        Some(to),
                        U256::from(1),
                        Bytes::from(input.clone()),
                        1_000,
                    )),
                },
                BinaryResponse::from_value(SimulationResult::EvmCall(EvmCallResult::new(
                    evm::ReceiptStatus::Success,
                    Bytes::from(output.clone()),
                    21,
                ))),
            )
            .await;

        let result = Call::do_handle_request(
            Arc::new(client),
            CallParams {
                call: CallObject {
                    from: Some(EthAddress::from(from)),
                    to: Some(EthAddress::from(to)),
                    data: Some(HexData::from(input)),
                    input: None,
                    value: Some(evm::EthU256::from(U256::from(1))),
                    gas: Some(evm::EthU256::from(1_000u64)),
                },
                block: BlockTag::Latest,
            },
        )
        .await
        .expect("eth_call should succeed");

        assert_eq!(result, HexData::from(output));
    }

    #[tokio::test]
    async fn eth_call_rejects_non_evm_simulation_result() {
        let client = BinaryPortMock::new();
        let from = evm::Address::new([1; evm::ADDRESS_LENGTH]);
        client
            .when_then(
                Command::Simulate {
                    request: SimulationRequest::EvmCall(EvmCallRequest::new(
                        from,
                        None,
                        U256::zero(),
                        Bytes::new(),
                        DEFAULT_ETH_CALL_GAS_LIMIT,
                    )),
                },
                BinaryResponse::from_value(SimulationResult::Transaction(
                    SpeculativeExecutionResult::example().clone(),
                )),
            )
            .await;

        let result = Call::do_handle_request(
            Arc::new(client),
            CallParams {
                call: CallObject {
                    from: Some(EthAddress::from(from)),
                    to: None,
                    data: None,
                    input: None,
                    value: None,
                    gas: None,
                },
                block: BlockTag::Latest,
            },
        )
        .await;

        assert!(matches!(
            result,
            Err(error) if error.code() == ReservedErrorCode::InternalError as i64
        ));
    }
}
