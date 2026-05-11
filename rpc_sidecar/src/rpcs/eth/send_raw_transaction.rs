use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use casper_json_rpc::{Error as RpcError, Params};
use casper_types::{TimeDiff, Timestamp, Transaction, evm};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    super::{NodeClient, RpcWithParams},
    types::{HexData, internal_error, invalid_params, parse_positional_params},
};
use crate::rpcs::docs::DocExample;

const DEFAULT_EVM_TX_TTL: TimeDiff = TimeDiff::from_seconds(300);

static SEND_RAW_TRANSACTION_PARAMS_EXAMPLE: LazyLock<SendRawTransactionParams> =
    LazyLock::new(|| SendRawTransactionParams {
        raw_transaction: HexData::from(Vec::new()),
    });

/// Params for `eth_sendRawTransaction`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SendRawTransactionParams {
    raw_transaction: HexData,
}

impl SendRawTransactionParams {
    fn raw_transaction(self) -> Vec<u8> {
        self.raw_transaction.into_bytes()
    }
}

impl DocExample for SendRawTransactionParams {
    fn doc_example() -> &'static Self {
        &SEND_RAW_TRANSACTION_PARAMS_EXAMPLE
    }
}

/// `eth_sendRawTransaction`.
pub struct SendRawTransaction;

#[async_trait]
impl RpcWithParams for SendRawTransaction {
    const METHOD: &'static str = "eth_sendRawTransaction";
    type RequestParams = SendRawTransactionParams;
    type ResponseResult = evm::Hash;

    fn try_parse_params(maybe_params: Option<Params>) -> Result<Self::RequestParams, RpcError> {
        let (raw_transaction,) = parse_positional_params::<(HexData,)>(maybe_params)?;
        Ok(SendRawTransactionParams { raw_transaction })
    }

    async fn do_handle_request(
        node_client: Arc<dyn NodeClient>,
        params: SendRawTransactionParams,
    ) -> Result<evm::Hash, RpcError> {
        let evm_transaction = evm::Transaction::from_signed_rlp(
            params.raw_transaction(),
            Timestamp::now(),
            DEFAULT_EVM_TX_TTL,
        )
        .map_err(|error| invalid_params(format!("invalid EVM transaction: {error}")))?;
        let hash = evm_transaction.hash();
        node_client
            .try_accept_transaction(Transaction::from(evm_transaction))
            .await
            .map_err(internal_error)?;
        Ok(hash.hash())
    }
}
