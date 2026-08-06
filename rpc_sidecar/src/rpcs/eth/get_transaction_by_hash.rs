use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use casper_json_rpc::{Error as RpcError, Params};
use casper_types::{
    BlockIdentifier, EvmTransactionHash, Timestamp, Transaction, TransactionHash, evm,
    execution::ExecutionResult,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    super::{NodeClient, RpcWithParams},
    transaction_response::{TransactionLocation, TransactionResponse, project_transaction},
    types::{block_hash_to_evm_hash, internal_error, parse_positional_params},
};
use crate::rpcs::docs::DocExample;

static GET_TRANSACTION_BY_HASH_PARAMS_EXAMPLE: LazyLock<GetTransactionByHashParams> =
    LazyLock::new(|| GetTransactionByHashParams {
        transaction_hash: evm::Hash::ZERO,
    });
static TRANSACTION_RESPONSE_EXAMPLE: LazyLock<Option<TransactionResponse>> = LazyLock::new(|| None);

/// Params for `eth_getTransactionByHash`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetTransactionByHashParams {
    transaction_hash: evm::Hash,
}

impl DocExample for GetTransactionByHashParams {
    fn doc_example() -> &'static Self {
        &GET_TRANSACTION_BY_HASH_PARAMS_EXAMPLE
    }
}

impl DocExample for Option<TransactionResponse> {
    fn doc_example() -> &'static Self {
        &TRANSACTION_RESPONSE_EXAMPLE
    }
}

/// `eth_getTransactionByHash`.
pub struct GetTransactionByHash;

#[async_trait]
impl RpcWithParams for GetTransactionByHash {
    const METHOD: &'static str = "eth_getTransactionByHash";
    type RequestParams = GetTransactionByHashParams;
    type ResponseResult = Option<TransactionResponse>;

    fn try_parse_params(maybe_params: Option<Params>) -> Result<Self::RequestParams, RpcError> {
        let (transaction_hash,) = parse_positional_params::<(evm::Hash,)>(maybe_params)?;
        Ok(GetTransactionByHashParams { transaction_hash })
    }

    async fn do_handle_request(
        node_client: Arc<dyn NodeClient>,
        params: GetTransactionByHashParams,
    ) -> Result<Option<TransactionResponse>, RpcError> {
        get_transaction_by_hash_at(node_client, params.transaction_hash, Timestamp::now()).await
    }
}

pub(crate) async fn get_transaction_by_hash_at(
    node_client: Arc<dyn NodeClient>,
    hash: evm::Hash,
    now: Timestamp,
) -> Result<Option<TransactionResponse>, RpcError> {
    let transaction_hash = TransactionHash::from(EvmTransactionHash::from_raw(hash.value()));
    let Some(transaction_with_info) = node_client
        .read_transaction_with_execution_info(transaction_hash, true)
        .await
        .map_err(internal_error)?
    else {
        return Ok(None);
    };
    let (transaction, maybe_execution_info) = transaction_with_info.into_inner();
    let expired = transaction.expired(now);
    let Transaction::Evm(transaction) = transaction else {
        return Err(internal_error(
            "EVM transaction hash resolved to non-EVM transaction",
        ));
    };
    if transaction.hash().hash() != hash {
        return Err(internal_error(
            "EVM transaction lookup returned a different stored transaction hash",
        ));
    }

    let Some(execution_info) = maybe_execution_info else {
        return if expired {
            Ok(None)
        } else {
            project_transaction(&transaction, TransactionLocation::Pending).map(Some)
        };
    };

    let Some(execution_result) = execution_info.execution_result else {
        return Err(internal_error(
            "block-included EVM transaction did not include an execution result",
        ));
    };
    let ExecutionResult::Evm(evm_execution_result) = execution_result else {
        return Err(internal_error(
            "block-included EVM transaction resolved to non-EVM execution result",
        ));
    };

    let Some(block_with_signatures) = node_client
        .read_block_with_signatures(Some(BlockIdentifier::Hash(execution_info.block_hash)))
        .await
        .map_err(internal_error)?
    else {
        return Err(internal_error(
            "block-included EVM transaction block was not found",
        ));
    };
    let block = block_with_signatures.block();
    if block.hash() != &execution_info.block_hash || block.height() != execution_info.block_height {
        return Err(internal_error(
            "block-included EVM transaction execution info does not match its block",
        ));
    }

    let transaction_index = block
        .all_transaction_hashes()
        .filter_map(|candidate| match candidate {
            TransactionHash::Evm(candidate) => Some(candidate),
            TransactionHash::Deploy(_) | TransactionHash::V1(_) => None,
        })
        .position(|candidate| candidate == transaction.hash())
        .ok_or_else(|| {
            internal_error("block-included EVM transaction was absent from its claimed block")
        })?;

    project_transaction(
        &transaction,
        TransactionLocation::BlockIncluded {
            block_hash: block_hash_to_evm_hash(execution_info.block_hash),
            block_number: execution_info.block_height,
            transaction_index,
            effective_gas_price: evm_execution_result.receipt.effective_gas_price,
        },
    )
    .map(Some)
}

#[cfg(test)]
mod tests {
    use alloy_consensus::{SignableTransaction, TxEip1559, TxEnvelope, crypto::secp256k1};
    use alloy_eips::{Encodable2718, eip2930::AccessList};
    use alloy_primitives::{Address as AlloyAddress, B256, Bytes as AlloyBytes, TxKind, U256};
    use casper_binary_port::{
        BinaryResponse, Command, InformationRequest, TransactionWithExecutionInfo,
    };
    use casper_json_rpc::ReservedErrorCode;
    use casper_types::{
        Block, BlockSignatures, BlockWithSignatures, Deploy, EvmTransaction, ExecutionInfo,
        TestBlockBuilder, TimeDiff,
        execution::{EvmExecutionResult, ExecutionResult},
        testing::TestRng,
    };
    use serde_json::json;

    use super::*;
    use crate::rpcs::test_utils::BinaryPortMock;

    #[tokio::test]
    async fn unknown_hash_returns_null() {
        let client = Arc::new(BinaryPortMock::new());
        add_transaction_response(&client, evm::Hash::ZERO, None).await;

        let response =
            get_transaction_by_hash_at(client.clone(), evm::Hash::ZERO, Timestamp::now())
                .await
                .unwrap();

        assert_eq!(response, None);
        client.verify_no_lingering().await;
    }

    #[tokio::test]
    async fn pending_transaction_is_visible_only_before_ttl_expiry() {
        let transaction = fixture_transaction();
        let hash = transaction.hash().hash();

        let live_client = Arc::new(BinaryPortMock::new());
        add_transaction_response(
            &live_client,
            hash,
            Some(TransactionWithExecutionInfo::new(
                Transaction::from(transaction.clone()),
                None,
            )),
        )
        .await;
        let live = get_transaction_by_hash_at(live_client.clone(), hash, Timestamp::from(300_999))
            .await
            .unwrap()
            .expect("pending transaction should still be visible");
        let live = serde_json::to_value(live).unwrap();
        assert_eq!(live["blockHash"], json!(null));
        assert_eq!(live["blockNumber"], json!(null));
        assert_eq!(live["transactionIndex"], json!(null));
        assert_eq!(live["gasPrice"], json!("0xbb8"));
        live_client.verify_no_lingering().await;

        let expired_client = Arc::new(BinaryPortMock::new());
        add_transaction_response(
            &expired_client,
            hash,
            Some(TransactionWithExecutionInfo::new(
                Transaction::from(transaction),
                None,
            )),
        )
        .await;
        let expired =
            get_transaction_by_hash_at(expired_client.clone(), hash, Timestamp::from(301_001))
                .await
                .unwrap();
        assert_eq!(expired, None);
        expired_client.verify_no_lingering().await;
    }

    #[tokio::test]
    async fn block_included_transaction_uses_evm_relative_index_and_survives_ttl() {
        let rng = &mut TestRng::new();
        let first_evm = fixture_transaction_with_nonce(2);
        let transaction = fixture_transaction();
        let native = Transaction::from(Deploy::random(rng));
        let transactions = vec![
            Transaction::from(first_evm),
            native,
            Transaction::from(transaction.clone()),
        ];
        let block = Block::V2(
            TestBlockBuilder::new()
                .height(42)
                .transactions(&transactions)
                .build(rng),
        );
        let mut evm_result = EvmExecutionResult::random(rng);
        evm_result.receipt.effective_gas_price = 1_500;
        let execution_info = ExecutionInfo {
            block_hash: *block.hash(),
            block_height: block.height(),
            execution_result: Some(ExecutionResult::Evm(Box::new(evm_result))),
        };
        let hash = transaction.hash().hash();
        let client = Arc::new(BinaryPortMock::new());
        add_transaction_response(
            &client,
            hash,
            Some(TransactionWithExecutionInfo::new(
                Transaction::from(transaction),
                Some(execution_info),
            )),
        )
        .await;
        client
            .add_block_with_signatures(
                BlockWithSignatures::new(block.clone(), BlockSignatures::random(rng)),
                InformationRequest::BlockWithSignatures(Some(BlockIdentifier::Hash(*block.hash()))),
            )
            .await;

        let response = get_transaction_by_hash_at(client.clone(), hash, Timestamp::from(u64::MAX))
            .await
            .unwrap()
            .expect("a block-included transaction remains visible after its TTL");
        let response = serde_json::to_value(response).unwrap();

        assert_eq!(
            response["blockHash"],
            json!(block_hash_to_evm_hash(block.hash()))
        );
        assert_eq!(response["blockNumber"], json!("0x2a"));
        assert_eq!(response["transactionIndex"], json!("0x1"));
        assert_eq!(response["gasPrice"], json!("0x5dc"));
        client.verify_no_lingering().await;
    }

    #[tokio::test]
    async fn block_included_transaction_requires_its_claimed_block() {
        let rng = &mut TestRng::new();
        let transaction = fixture_transaction();
        let hash = transaction.hash().hash();
        let mut evm_result = EvmExecutionResult::random(rng);
        evm_result.receipt.effective_gas_price = 1_500;
        let block_hash = casper_types::BlockHash::random(rng);
        let client = Arc::new(BinaryPortMock::new());
        add_transaction_response(
            &client,
            hash,
            Some(TransactionWithExecutionInfo::new(
                Transaction::from(transaction),
                Some(ExecutionInfo {
                    block_hash,
                    block_height: 42,
                    execution_result: Some(ExecutionResult::Evm(Box::new(evm_result))),
                }),
            )),
        )
        .await;
        let request =
            InformationRequest::BlockWithSignatures(Some(BlockIdentifier::Hash(block_hash)))
                .try_into()
                .unwrap();
        client
            .when_then(
                Command::Get(request),
                BinaryResponse::from_option(None::<BlockWithSignatures>),
            )
            .await;

        let error = get_transaction_by_hash_at(client.clone(), hash, Timestamp::now())
            .await
            .expect_err("missing claimed block must be an internal consistency error");

        assert_eq!(
            error,
            RpcError::new(
                ReservedErrorCode::InternalError,
                "block-included EVM transaction block was not found",
            )
        );
        client.verify_no_lingering().await;
    }

    #[tokio::test]
    async fn block_included_transaction_requires_an_execution_result() {
        let rng = &mut TestRng::new();
        let transaction = fixture_transaction();
        let hash = transaction.hash().hash();
        let client = Arc::new(BinaryPortMock::new());
        add_transaction_response(
            &client,
            hash,
            Some(TransactionWithExecutionInfo::new(
                Transaction::from(transaction),
                Some(ExecutionInfo {
                    block_hash: casper_types::BlockHash::random(rng),
                    block_height: 42,
                    execution_result: None,
                }),
            )),
        )
        .await;

        let error = get_transaction_by_hash_at(client.clone(), hash, Timestamp::now())
            .await
            .expect_err("missing execution result must be an internal consistency error");

        assert_eq!(
            error,
            RpcError::new(
                ReservedErrorCode::InternalError,
                "block-included EVM transaction did not include an execution result",
            )
        );
        client.verify_no_lingering().await;
    }

    #[tokio::test]
    async fn block_included_transaction_must_be_present_in_its_claimed_block() {
        let rng = &mut TestRng::new();
        let transaction = fixture_transaction();
        let hash = transaction.hash().hash();
        let block = Block::V2(TestBlockBuilder::new().height(42).build(rng));
        let client = Arc::new(BinaryPortMock::new());
        add_transaction_response(
            &client,
            hash,
            Some(TransactionWithExecutionInfo::new(
                Transaction::from(transaction),
                Some(ExecutionInfo {
                    block_hash: *block.hash(),
                    block_height: block.height(),
                    execution_result: Some(ExecutionResult::Evm(Box::new(
                        EvmExecutionResult::random(rng),
                    ))),
                }),
            )),
        )
        .await;
        client
            .add_block_with_signatures(
                BlockWithSignatures::new(block.clone(), BlockSignatures::random(rng)),
                InformationRequest::BlockWithSignatures(Some(BlockIdentifier::Hash(*block.hash()))),
            )
            .await;

        let error = get_transaction_by_hash_at(client.clone(), hash, Timestamp::now())
            .await
            .expect_err("transaction absent from claimed block must fail");

        assert_eq!(
            error,
            RpcError::new(
                ReservedErrorCode::InternalError,
                "block-included EVM transaction was absent from its claimed block",
            )
        );
        client.verify_no_lingering().await;
    }

    #[test]
    fn validates_transaction_hash_parameter_shape() {
        assert!(GetTransactionByHash::try_parse_params(None).is_err());
        assert!(
            GetTransactionByHash::try_parse_params(Some(Params::Array(vec![json!("0x1234")])))
                .is_err()
        );
        assert!(
            GetTransactionByHash::try_parse_params(Some(Params::Array(vec![json!(format!(
                "0x{}",
                "00".repeat(33)
            ))])))
            .is_err()
        );
    }

    async fn add_transaction_response(
        client: &BinaryPortMock,
        hash: evm::Hash,
        response: Option<TransactionWithExecutionInfo>,
    ) {
        let transaction_hash = TransactionHash::from(EvmTransactionHash::from_raw(hash.value()));
        let request = InformationRequest::Transaction {
            hash: transaction_hash,
            with_finalized_approvals: true,
        }
        .try_into()
        .unwrap();
        client
            .when_then(Command::Get(request), BinaryResponse::from_option(response))
            .await;
    }

    fn fixture_transaction() -> EvmTransaction {
        fixture_transaction_with_nonce(1)
    }

    fn fixture_transaction_with_nonce(nonce: u64) -> EvmTransaction {
        let transaction = TxEip1559 {
            chain_id: 7,
            nonce,
            gas_limit: 60_000,
            max_fee_per_gas: 3_000,
            max_priority_fee_per_gas: 0,
            to: TxKind::Call(AlloyAddress::from([0x55; 20])),
            value: U256::from(13),
            access_list: AccessList::default(),
            input: AlloyBytes::from(vec![0xbe, 0xef]),
        };
        let signature =
            secp256k1::sign_message(B256::from([7; 32]), transaction.signature_hash()).unwrap();
        let envelope: TxEnvelope = transaction.into_signed(signature).into();
        EvmTransaction::from_signed_rlp(
            envelope.encoded_2718(),
            Timestamp::from(1_000),
            TimeDiff::from_seconds(300),
        )
        .unwrap()
    }
}
