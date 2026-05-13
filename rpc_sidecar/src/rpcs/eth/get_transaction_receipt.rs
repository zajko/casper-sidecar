use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use casper_json_rpc::{Error as RpcError, Params};
use casper_types::{
    BlockHash, BlockIdentifier, Transaction, TransactionHash, evm, execution::ExecutionResult,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    super::{NodeClient, RpcWithParams},
    types::{
        EthAddress, HexData, block_hash_to_evm_hash, internal_error, logs_bloom,
        parse_positional_params,
    },
};
use crate::rpcs::docs::DocExample;

static GET_TRANSACTION_RECEIPT_PARAMS_EXAMPLE: LazyLock<GetTransactionReceiptParams> =
    LazyLock::new(|| GetTransactionReceiptParams {
        transaction_hash: evm::Hash::ZERO,
    });
static RECEIPT_EXAMPLE: LazyLock<Option<TransactionReceiptResponse>> = LazyLock::new(|| None);

/// Params for `eth_getTransactionReceipt`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GetTransactionReceiptParams {
    transaction_hash: evm::Hash,
}

impl GetTransactionReceiptParams {
    fn transaction_hash(&self) -> evm::Hash {
        self.transaction_hash
    }
}

impl DocExample for GetTransactionReceiptParams {
    fn doc_example() -> &'static Self {
        &GET_TRANSACTION_RECEIPT_PARAMS_EXAMPLE
    }
}

/// Ethereum log response entry.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LogResponse {
    address: EthAddress,
    topics: Vec<evm::Hash>,
    data: HexData,
    block_hash: evm::Hash,
    block_number: evm::EthU256,
    transaction_hash: evm::Hash,
    transaction_index: evm::EthU256,
    log_index: evm::EthU256,
    removed: bool,
}

/// Ethereum transaction receipt response.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TransactionReceiptResponse {
    #[serde(rename = "type")]
    transaction_type: evm::EthU256,
    transaction_hash: evm::Hash,
    block_hash: evm::Hash,
    block_number: evm::EthU256,
    from: EthAddress,
    to: Option<EthAddress>,
    contract_address: Option<EthAddress>,
    status: evm::EthU256,
    gas_used: evm::EthU256,
    effective_gas_price: evm::EthU256,
    logs: Vec<LogResponse>,
    logs_bloom: HexData,
    transaction_index: evm::EthU256,
    cumulative_gas_used: evm::EthU256,
}

impl DocExample for Option<TransactionReceiptResponse> {
    fn doc_example() -> &'static Self {
        &RECEIPT_EXAMPLE
    }
}

/// `eth_getTransactionReceipt`.
pub struct GetTransactionReceipt;

#[async_trait]
impl RpcWithParams for GetTransactionReceipt {
    const METHOD: &'static str = "eth_getTransactionReceipt";
    type RequestParams = GetTransactionReceiptParams;
    type ResponseResult = Option<TransactionReceiptResponse>;

    fn try_parse_params(maybe_params: Option<Params>) -> Result<Self::RequestParams, RpcError> {
        let (transaction_hash,) = parse_positional_params::<(evm::Hash,)>(maybe_params)?;
        Ok(GetTransactionReceiptParams { transaction_hash })
    }

    async fn do_handle_request(
        node_client: Arc<dyn NodeClient>,
        params: GetTransactionReceiptParams,
    ) -> Result<Option<TransactionReceiptResponse>, RpcError> {
        let hash = params.transaction_hash();
        let transaction_hash = TransactionHash::from(evm::TransactionHash::from_raw(hash.value()));
        let Some(transaction_with_info) = node_client
            .read_transaction_with_execution_info(transaction_hash, true)
            .await
            .map_err(internal_error)?
        else {
            return Ok(None);
        };
        let (transaction, maybe_execution_info) = transaction_with_info.into_inner();
        let Some(execution_info) = maybe_execution_info else {
            return Ok(None);
        };
        let Some(execution_result) = execution_info.execution_result else {
            return Ok(None);
        };
        let Transaction::Evm(evm_transaction) = transaction else {
            return Err(internal_error(
                "transaction hash did not resolve to EVM transaction",
            ));
        };
        let ExecutionResult::Evm(evm_execution_result) = execution_result else {
            return Err(internal_error(
                "EVM transaction did not resolve to EVM execution result",
            ));
        };

        let block = node_client
            .read_block_with_signatures(Some(BlockIdentifier::Hash(execution_info.block_hash)))
            .await
            .map_err(internal_error)?
            .ok_or_else(|| internal_error("receipt block was not found"))?;
        let block_hashes = block
            .block()
            .all_transaction_hashes()
            .collect::<Vec<TransactionHash>>();
        // Casper block order may include non-EVM transactions. Keep the raw
        // Casper index for prior receipt aggregation, but report indexes in
        // the filtered Ethereum transaction list exposed by eth_getBlockByNumber.
        let (block_transaction_index, transaction_index) =
            transaction_indexes(&block_hashes, transaction_hash)?;
        let (prior_gas_used, prior_log_count) =
            prior_evm_receipt_totals(node_client, &block_hashes[..block_transaction_index]).await?;
        let receipt = &evm_execution_result.receipt;
        let cumulative_gas_used = prior_gas_used.saturating_add(receipt.gas_used);
        let logs = receipt
            .logs
            .iter()
            .enumerate()
            .map(|(offset, log)| {
                receipt_log_response(
                    log,
                    execution_info.block_hash,
                    execution_info.block_height,
                    hash,
                    transaction_index,
                    prior_log_count + offset,
                )
            })
            .collect::<Vec<LogResponse>>();

        Ok(Some(TransactionReceiptResponse {
            transaction_type: evm::EthU256::from(evm_transaction.kind().type_id()),
            transaction_hash: hash,
            block_hash: block_hash_to_evm_hash(execution_info.block_hash),
            block_number: evm::EthU256::from(execution_info.block_height),
            from: EthAddress::from(evm_transaction.from()),
            to: evm_transaction.to().map(EthAddress::from),
            contract_address: receipt.contract_address.map(EthAddress::from),
            status: evm::EthU256::from(receipt.status.eth_status()),
            gas_used: evm::EthU256::from(receipt.gas_used),
            effective_gas_price: evm::EthU256::from(receipt.effective_gas_price),
            logs,
            logs_bloom: HexData::from(logs_bloom(&receipt.logs).as_slice()),
            transaction_index: evm::EthU256::from(transaction_index),
            cumulative_gas_used: evm::EthU256::from(cumulative_gas_used),
        }))
    }
}

fn transaction_indexes(
    block_hashes: &[TransactionHash],
    transaction_hash: TransactionHash,
) -> Result<(usize, usize), RpcError> {
    let mut evm_transaction_index = 0usize;

    for (block_transaction_index, candidate) in block_hashes.iter().copied().enumerate() {
        let TransactionHash::Evm(_) = candidate else {
            continue;
        };
        if candidate == transaction_hash {
            return Ok((block_transaction_index, evm_transaction_index));
        }
        evm_transaction_index += 1;
    }

    Err(internal_error(
        "receipt block does not contain EVM transaction hash",
    ))
}

async fn prior_evm_receipt_totals(
    node_client: Arc<dyn NodeClient>,
    hashes: &[TransactionHash],
) -> Result<(u64, usize), RpcError> {
    let mut gas_used = 0u64;
    let mut log_count = 0usize;
    for hash in hashes {
        let Some(transaction_with_info) = node_client
            .read_transaction_with_execution_info(*hash, true)
            .await
            .map_err(internal_error)?
        else {
            continue;
        };
        let (_, Some(execution_info)) = transaction_with_info.into_inner() else {
            continue;
        };
        let Some(ExecutionResult::Evm(result)) = execution_info.execution_result else {
            continue;
        };
        gas_used = gas_used.saturating_add(result.receipt.gas_used);
        log_count = log_count.saturating_add(result.receipt.logs.len());
    }
    Ok((gas_used, log_count))
}

fn receipt_log_response(
    log: &evm::Log,
    block_hash: BlockHash,
    block_number: u64,
    transaction_hash: evm::Hash,
    transaction_index: usize,
    log_index: usize,
) -> LogResponse {
    LogResponse {
        address: EthAddress::from(log.address),
        topics: log.topics.clone(),
        data: HexData::from(log.data.as_ref()),
        block_hash: block_hash_to_evm_hash(block_hash),
        block_number: evm::EthU256::from(block_number),
        transaction_hash,
        transaction_index: evm::EthU256::from(transaction_index),
        log_index: evm::EthU256::from(log_index),
        removed: false,
    }
}

#[cfg(test)]
mod tests {
    use casper_types::{DeployHash, Digest, TransactionV1Hash};

    use super::*;

    #[test]
    fn transaction_indexes_reports_evm_filtered_index() {
        let deploy = TransactionHash::from(DeployHash::new(Digest::from_raw([1; 32])));
        let evm_a = TransactionHash::from(evm::TransactionHash::from_raw([2; evm::HASH_LENGTH]));
        let v1 = TransactionHash::from(TransactionV1Hash::from_raw([3; 32]));
        let evm_b = TransactionHash::from(evm::TransactionHash::from_raw([4; evm::HASH_LENGTH]));
        let block_hashes = [deploy, evm_a, v1, evm_b];

        assert_eq!(transaction_indexes(&block_hashes, evm_a).unwrap(), (1, 0));
        assert_eq!(transaction_indexes(&block_hashes, evm_b).unwrap(), (3, 1));
    }
}
