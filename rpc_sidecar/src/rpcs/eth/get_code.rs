use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use casper_json_rpc::{Error as RpcError, Params};
use casper_types::{EvmAddr, GlobalStateIdentifier, Key, StoredValue, evm};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    super::{NodeClient, RpcWithParams},
    types::{
        BlockNumberParam, BlockTag, EthAddress, HexData, internal_error, parse_positional_params,
    },
};
use crate::rpcs::docs::DocExample;

static GET_CODE_PARAMS_EXAMPLE: LazyLock<GetCodeParams> = LazyLock::new(|| GetCodeParams {
    address: EthAddress::from(evm::Address::ZERO),
    block: BlockNumberParam::Tag(BlockTag::Latest),
});

/// Params for `eth_getCode`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetCodeParams {
    address: EthAddress,
    block: BlockNumberParam,
}

impl GetCodeParams {
    fn address(&self) -> evm::Address {
        self.address.into_inner()
    }

    fn state_identifier(&self) -> Result<Option<GlobalStateIdentifier>, RpcError> {
        self.block
            .identifier()
            .map(|maybe_identifier| maybe_identifier.map(GlobalStateIdentifier::from))
    }
}

impl DocExample for GetCodeParams {
    fn doc_example() -> &'static Self {
        &GET_CODE_PARAMS_EXAMPLE
    }
}

#[derive(Deserialize)]
struct PositionalParams(EthAddress, BlockNumberParam);

impl From<PositionalParams> for GetCodeParams {
    fn from(params: PositionalParams) -> Self {
        GetCodeParams {
            address: params.0,
            block: params.1,
        }
    }
}

async fn read_code(
    node_client: &dyn NodeClient,
    state_identifier: Option<GlobalStateIdentifier>,
    address: evm::Address,
) -> Result<HexData, RpcError> {
    let code_hash_key = Key::Evm(EvmAddr::CodeHash(address));
    let maybe_code_hash = node_client
        .query_global_state(state_identifier, code_hash_key, vec![])
        .await
        .map_err(internal_error)?;
    let code_hash = match maybe_code_hash.map(|value| value.into_inner().0) {
        Some(StoredValue::CLValue(cl_value)) => {
            cl_value.into_t::<evm::Hash>().map_err(|error| {
                internal_error(format!(
                    "invalid EVM code hash under {code_hash_key}: {error}"
                ))
            })?
        }
        Some(other) => {
            return Err(internal_error(format!(
                "expected EVM code hash CLValue under {code_hash_key}, found {}",
                other.type_name()
            )));
        }
        None => return Ok(HexData::default()),
    };

    if code_hash == evm::EMPTY_CODE_HASH {
        return Ok(HexData::default());
    }

    let byte_code_key = Key::Evm(EvmAddr::ByteCode(code_hash));
    let maybe_byte_code = node_client
        .query_global_state(state_identifier, byte_code_key, vec![])
        .await
        .map_err(internal_error)?;
    match maybe_byte_code.map(|value| value.into_inner().0) {
        Some(StoredValue::ByteCode(byte_code)) if byte_code.kind().is_evm() => {
            Ok(HexData::from(byte_code.take_bytes()))
        }
        Some(StoredValue::ByteCode(byte_code)) => Err(internal_error(format!(
            "expected EVM bytecode under {byte_code_key}, found {} bytecode",
            byte_code.kind()
        ))),
        Some(other) => Err(internal_error(format!(
            "expected EVM bytecode under {byte_code_key}, found {}",
            other.type_name()
        ))),
        // This matches the EVM executor's missing-bytecode behavior.
        None => Ok(HexData::default()),
    }
}

/// `eth_getCode`.
pub struct GetCode;

#[async_trait]
impl RpcWithParams for GetCode {
    const METHOD: &'static str = "eth_getCode";
    type RequestParams = GetCodeParams;
    type ResponseResult = HexData;

    fn try_parse_params(maybe_params: Option<Params>) -> Result<Self::RequestParams, RpcError> {
        parse_positional_params::<PositionalParams>(maybe_params).map(Into::into)
    }

    async fn do_handle_request(
        node_client: Arc<dyn NodeClient>,
        params: GetCodeParams,
    ) -> Result<HexData, RpcError> {
        read_code(
            node_client.as_ref(),
            params.state_identifier()?,
            params.address(),
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use casper_binary_port::{
        BinaryResponse, Command, GetRequest, GlobalStateEntityQualifier, GlobalStateQueryResult,
        GlobalStateRequest,
    };
    use casper_json_rpc::ReservedErrorCode;
    use casper_types::{ByteCode, ByteCodeKind, CLValue};
    use serde_json::json;

    use super::*;
    use crate::rpcs::test_utils::BinaryPortMock;

    const BLOCK_HEIGHT: u64 = 69;

    #[tokio::test]
    async fn reads_evm_bytecode_at_numeric_height() {
        let client = BinaryPortMock::new();
        let address = evm::Address::new([1; evm::ADDRESS_LENGTH]);
        let code_hash = evm::Hash::new([2; evm::HASH_LENGTH]);
        let state_identifier = Some(GlobalStateIdentifier::BlockHeight(BLOCK_HEIGHT));
        add_state_response(
            &client,
            state_identifier,
            Key::Evm(EvmAddr::CodeHash(address)),
            Some(StoredValue::CLValue(CLValue::from_t(code_hash).unwrap())),
        )
        .await;
        add_state_response(
            &client,
            state_identifier,
            Key::Evm(EvmAddr::ByteCode(code_hash)),
            Some(StoredValue::ByteCode(ByteCode::new(
                ByteCodeKind::EvmPrague,
                vec![0x60, 0x00],
            ))),
        )
        .await;

        let result = GetCode::do_handle_request(
            Arc::new(client),
            GetCodeParams {
                address: address.into(),
                block: BlockNumberParam::Height(BLOCK_HEIGHT.into()),
            },
        )
        .await
        .expect("code lookup should succeed");

        assert_eq!(serde_json::to_value(result).unwrap(), json!("0x6000"));
    }

    #[tokio::test]
    async fn returns_empty_code_for_unknown_address() {
        let client = BinaryPortMock::new();
        let address = evm::Address::new([3; evm::ADDRESS_LENGTH]);
        add_state_response(&client, None, Key::Evm(EvmAddr::CodeHash(address)), None).await;

        let result = GetCode::do_handle_request(
            Arc::new(client),
            GetCodeParams {
                address: address.into(),
                block: BlockNumberParam::Tag(BlockTag::Latest),
            },
        )
        .await
        .expect("unknown address should have empty code");

        assert_eq!(serde_json::to_value(result).unwrap(), json!("0x"));
    }

    #[tokio::test]
    async fn empty_code_hash_does_not_query_bytecode() {
        let client = BinaryPortMock::new();
        let address = evm::Address::new([4; evm::ADDRESS_LENGTH]);
        add_state_response(
            &client,
            None,
            Key::Evm(EvmAddr::CodeHash(address)),
            Some(StoredValue::CLValue(
                CLValue::from_t(evm::EMPTY_CODE_HASH).unwrap(),
            )),
        )
        .await;

        let result = GetCode::do_handle_request(
            Arc::new(client),
            GetCodeParams {
                address: address.into(),
                block: BlockNumberParam::Tag(BlockTag::Pending),
            },
        )
        .await
        .expect("empty code hash should return empty code");

        assert_eq!(serde_json::to_value(result).unwrap(), json!("0x"));
    }

    #[tokio::test]
    async fn rejects_non_evm_bytecode() {
        let client = BinaryPortMock::new();
        let address = evm::Address::new([5; evm::ADDRESS_LENGTH]);
        let code_hash = evm::Hash::new([6; evm::HASH_LENGTH]);
        add_state_response(
            &client,
            None,
            Key::Evm(EvmAddr::CodeHash(address)),
            Some(StoredValue::CLValue(CLValue::from_t(code_hash).unwrap())),
        )
        .await;
        add_state_response(
            &client,
            None,
            Key::Evm(EvmAddr::ByteCode(code_hash)),
            Some(StoredValue::ByteCode(ByteCode::new(
                ByteCodeKind::V1CasperWasm,
                vec![0x00],
            ))),
        )
        .await;

        let error = GetCode::do_handle_request(
            Arc::new(client),
            GetCodeParams {
                address: address.into(),
                block: BlockNumberParam::Tag(BlockTag::Finalized),
            },
        )
        .await
        .expect_err("non-EVM bytecode should fail");

        assert_eq!(error.code(), ReservedErrorCode::InternalError as i64);
    }

    #[test]
    fn parses_metamask_numeric_block_selector() {
        let address = format!("0x{}", "07".repeat(evm::ADDRESS_LENGTH));
        let params =
            GetCode::try_parse_params(Some(Params::Array(vec![json!(address), json!("0x45")])))
                .expect("MetaMask get-code request should parse");

        assert_eq!(
            params.state_identifier().unwrap(),
            Some(GlobalStateIdentifier::BlockHeight(BLOCK_HEIGHT))
        );
    }

    async fn add_state_response(
        client: &BinaryPortMock,
        state_identifier: Option<GlobalStateIdentifier>,
        key: Key,
        value: Option<StoredValue>,
    ) {
        let request = GlobalStateRequest::new(
            state_identifier,
            GlobalStateEntityQualifier::Item {
                base_key: key,
                path: Vec::new(),
            },
        );
        client
            .when_then(
                Command::Get(GetRequest::State(Box::new(request))),
                BinaryResponse::from_option(
                    value.map(|value| GlobalStateQueryResult::new(value, Vec::new())),
                ),
            )
            .await;
    }
}
