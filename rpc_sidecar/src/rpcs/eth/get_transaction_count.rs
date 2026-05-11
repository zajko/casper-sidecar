use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use casper_json_rpc::{Error as RpcError, Params};
use casper_types::{Key, StoredValue, evm};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    super::{NodeClient, RpcWithParams},
    types::{BlockTag, EthAddress, internal_error, parse_positional_params},
};
use crate::rpcs::docs::DocExample;

static GET_TRANSACTION_COUNT_PARAMS_EXAMPLE: LazyLock<GetTransactionCountParams> =
    LazyLock::new(|| GetTransactionCountParams {
        address: EthAddress::from(evm::Address::ZERO),
        block: BlockTag::Latest,
    });

/// Params for `eth_getTransactionCount`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetTransactionCountParams {
    address: EthAddress,
    #[serde(default)]
    block: BlockTag,
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
struct PositionalParams(EthAddress, #[serde(default)] BlockTag);

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
    type ResponseResult = evm::EthU256;

    fn try_parse_params(maybe_params: Option<Params>) -> Result<Self::RequestParams, RpcError> {
        parse_positional_params::<PositionalParams>(maybe_params).map(Into::into)
    }

    async fn do_handle_request(
        node_client: Arc<dyn NodeClient>,
        params: GetTransactionCountParams,
    ) -> Result<evm::EthU256, RpcError> {
        let address = params.address();
        let maybe_value = node_client
            .query_global_state(None, Key::EvmAccount(address), vec![])
            .await
            .map_err(internal_error)?;
        let nonce = match maybe_value.map(|value| value.into_inner().0) {
            Some(StoredValue::EvmAccount(account)) => account.nonce(),
            Some(other) => {
                return Err(internal_error(format!(
                    "expected EVM account under key, found {}",
                    other.type_name()
                )));
            }
            None => 0,
        };
        Ok(evm::EthU256::from(nonce))
    }
}

#[cfg(test)]
mod tests {
    use casper_binary_port::{
        BinaryResponse, Command, GetRequest, GlobalStateEntityQualifier, GlobalStateQueryResult,
        GlobalStateRequest,
    };

    use super::*;
    use crate::rpcs::test_utils::BinaryPortMock;

    #[tokio::test]
    async fn get_transaction_count_reads_evm_account_nonce() {
        let client = BinaryPortMock::new();
        let address = evm::Address::new([1; evm::ADDRESS_LENGTH]);
        let account = evm::Account::new(12, evm::Hash::ZERO, evm::deterministic_purse(address));
        let request = GlobalStateRequest::new(
            None,
            GlobalStateEntityQualifier::Item {
                base_key: Key::EvmAccount(address),
                path: Vec::new(),
            },
        );
        client
            .when_then(
                Command::Get(GetRequest::State(Box::new(request))),
                BinaryResponse::from_option(Some(GlobalStateQueryResult::new(
                    StoredValue::EvmAccount(account),
                    Vec::new(),
                ))),
            )
            .await;

        let result = GetTransactionCount::do_handle_request(
            Arc::new(client),
            GetTransactionCountParams {
                address: EthAddress::from(address),
                block: BlockTag::Latest,
            },
        )
        .await
        .expect("nonce lookup should succeed");

        assert_eq!(result, evm::EthU256::from(12u64));
    }
}
