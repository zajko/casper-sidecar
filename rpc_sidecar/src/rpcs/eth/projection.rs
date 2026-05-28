use std::sync::Arc;

use alloy_consensus::{
    Eip658Value, Receipt as AlloyReceipt, ReceiptEnvelope, ReceiptWithBloom, TxType,
    proofs::calculate_receipt_root,
};
use alloy_primitives::{
    Address as AlloyAddress, B256, Bloom, Bytes as AlloyBytes, Log as AlloyLog,
};
use casper_json_rpc::Error as RpcError;
use casper_types::{
    BlockHash, BlockIdentifier, Digest, EvmTransactionHash, Transaction, TransactionHash, evm,
    execution::ExecutionResult,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    super::NodeClient,
    eth_u256::EthU256,
    types::{EthAddress, HexData, block_hash_to_evm_hash, internal_error},
};

/// Ethereum log response entry.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LogResponse {
    pub(crate) address: EthAddress,
    pub(crate) topics: Vec<evm::Topic>,
    pub(crate) data: HexData,
    pub(crate) block_hash: evm::Hash,
    pub(crate) block_number: EthU256,
    pub(crate) transaction_hash: evm::Hash,
    pub(crate) transaction_index: EthU256,
    pub(crate) log_index: EthU256,
    pub(crate) removed: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct ProjectedReceipt {
    pub(crate) transaction_type: EthU256,
    pub(crate) transaction_hash: evm::Hash,
    pub(crate) block_hash: evm::Hash,
    pub(crate) block_number: EthU256,
    pub(crate) from: EthAddress,
    pub(crate) to: Option<EthAddress>,
    pub(crate) contract_address: Option<EthAddress>,
    pub(crate) status: EthU256,
    pub(crate) gas_used: EthU256,
    pub(crate) effective_gas_price: EthU256,
    pub(crate) logs: Vec<LogResponse>,
    pub(crate) logs_bloom: HexData,
    pub(crate) transaction_index: EthU256,
    pub(crate) cumulative_gas_used: EthU256,
}

#[derive(Clone, Debug)]
pub(crate) struct ProjectedBlock {
    pub(crate) number: EthU256,
    pub(crate) hash: evm::Hash,
    pub(crate) parent_hash: evm::Hash,
    pub(crate) transactions_root: evm::Hash,
    pub(crate) state_root: evm::Hash,
    pub(crate) receipts_root: evm::Hash,
    pub(crate) logs_bloom: HexData,
    pub(crate) gas_used: EthU256,
    pub(crate) timestamp: EthU256,
    pub(crate) transactions: Vec<evm::Hash>,
    pub(crate) receipts: Vec<ProjectedReceipt>,
}

impl ProjectedBlock {
    pub(crate) fn receipt(&self, transaction_hash: evm::Hash) -> Option<&ProjectedReceipt> {
        self.receipts
            .iter()
            .find(|receipt| receipt.transaction_hash == transaction_hash)
    }

    pub(crate) fn logs(&self) -> impl Iterator<Item = &LogResponse> {
        self.receipts.iter().flat_map(|receipt| receipt.logs.iter())
    }
}

pub(crate) async fn project_transaction_receipt(
    node_client: Arc<dyn NodeClient>,
    hash: evm::Hash,
) -> Result<Option<(evm::Hash, ProjectedReceipt)>, RpcError> {
    let transaction_hash = TransactionHash::from(EvmTransactionHash::from_raw(hash.value()));
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
    if !matches!(transaction, Transaction::Evm(_)) {
        return Err(internal_error(
            "transaction hash did not resolve to EVM transaction",
        ));
    }
    let Some(ExecutionResult::Evm(_)) = execution_info.execution_result else {
        return Err(internal_error(
            "EVM transaction did not resolve to EVM execution result",
        ));
    };

    let block = project_block(
        node_client,
        Some(BlockIdentifier::Hash(execution_info.block_hash)),
    )
    .await?
    .ok_or_else(|| internal_error("receipt block was not found"))?;
    let receipt = block
        .receipt(hash)
        .cloned()
        .ok_or_else(|| internal_error("receipt block does not contain EVM transaction hash"))?;
    Ok(Some((block.hash, receipt)))
}

pub(crate) async fn project_block(
    node_client: Arc<dyn NodeClient>,
    identifier: Option<BlockIdentifier>,
) -> Result<Option<ProjectedBlock>, RpcError> {
    let Some(block_with_signatures) = node_client
        .read_block_with_signatures(identifier)
        .await
        .map_err(internal_error)?
    else {
        return Ok(None);
    };
    let block = block_with_signatures.block();
    let block_hash = block_hash_to_evm_hash(block.hash());
    let block_number = block.height();
    let block_hashes = block
        .all_transaction_hashes()
        .collect::<Vec<TransactionHash>>();
    let evm_hashes = block_hashes
        .iter()
        .copied()
        .filter_map(|hash| match hash {
            TransactionHash::Evm(hash) => Some(hash),
            TransactionHash::Deploy(_) | TransactionHash::V1(_) => None,
        })
        .collect::<Vec<_>>();

    let mut cumulative_gas_used = 0u64;
    let mut log_index = 0usize;
    let mut block_alloy_logs = Vec::new();
    let mut alloy_receipts = Vec::new();
    let mut receipts = Vec::new();
    let mut transactions = Vec::new();

    for (transaction_index, evm_transaction_hash) in evm_hashes.iter().copied().enumerate() {
        let transaction_hash = TransactionHash::from(evm_transaction_hash);
        let Some(transaction_with_info) = node_client
            .read_transaction_with_execution_info(transaction_hash, true)
            .await
            .map_err(internal_error)?
        else {
            return Err(internal_error(
                "block EVM transaction was not found by transaction hash",
            ));
        };
        let (transaction, maybe_execution_info) = transaction_with_info.into_inner();
        let Some(execution_info) = maybe_execution_info else {
            return Err(internal_error(
                "block EVM transaction did not include execution info",
            ));
        };
        let Some(execution_result) = execution_info.execution_result else {
            return Err(internal_error(
                "block EVM transaction did not include execution result",
            ));
        };
        let Transaction::Evm(evm_transaction) = transaction else {
            return Err(internal_error(
                "block EVM transaction hash resolved to non-EVM transaction",
            ));
        };
        let ExecutionResult::Evm(evm_execution_result) = execution_result else {
            return Err(internal_error(
                "block EVM transaction resolved to non-EVM execution result",
            ));
        };

        let receipt = &evm_execution_result.receipt;
        cumulative_gas_used = cumulative_gas_used.saturating_add(receipt.gas_used);
        let transaction_hash = evm_transaction_hash.hash();
        let logs = receipt
            .logs
            .iter()
            .enumerate()
            .map(|(offset, log)| {
                receipt_log_response(
                    log,
                    *block.hash(),
                    block_number,
                    transaction_hash,
                    transaction_index,
                    log_index + offset,
                )
            })
            .collect::<Vec<_>>();
        log_index = log_index.saturating_add(logs.len());

        let alloy_logs = receipt
            .logs
            .iter()
            .map(alloy_log_from_evm_log)
            .collect::<Vec<_>>();
        let receipt_bloom = alloy_primitives::logs_bloom(&alloy_logs);
        block_alloy_logs.extend(alloy_logs.iter().cloned());
        let alloy_receipt = ReceiptWithBloom {
            receipt: AlloyReceipt {
                status: Eip658Value::Eip658(receipt.status.is_success()),
                cumulative_gas_used,
                logs: alloy_logs,
            },
            logs_bloom: receipt_bloom,
        };
        alloy_receipts.push(receipt_envelope(
            evm_transaction.kind().type_id(),
            alloy_receipt,
        )?);

        transactions.push(transaction_hash);
        receipts.push(ProjectedReceipt {
            transaction_type: EthU256::from(evm_transaction.kind().type_id()),
            transaction_hash,
            block_hash,
            block_number: EthU256::from(block_number),
            from: EthAddress::from(evm_transaction.from()),
            to: evm_transaction.to().map(EthAddress::from),
            contract_address: receipt.contract_address.map(EthAddress::from),
            status: EthU256::from(receipt.status.eth_status()),
            gas_used: EthU256::from(receipt.gas_used),
            effective_gas_price: EthU256::from(receipt.effective_gas_price),
            logs,
            logs_bloom: bloom_hex(&receipt_bloom),
            transaction_index: EthU256::from(transaction_index),
            cumulative_gas_used: EthU256::from(cumulative_gas_used),
        });
    }

    let receipts_root = calculate_receipt_root(&alloy_receipts);
    let block_bloom = alloy_primitives::logs_bloom(&block_alloy_logs);

    Ok(Some(ProjectedBlock {
        number: EthU256::from(block_number),
        hash: block_hash,
        parent_hash: block_hash_to_evm_hash(block.parent_hash()),
        transactions_root: digest_to_evm_hash(block.body_hash()),
        state_root: digest_to_evm_hash(block.state_root_hash()),
        receipts_root: b256_to_evm_hash(receipts_root),
        logs_bloom: bloom_hex(&block_bloom),
        gas_used: EthU256::from(cumulative_gas_used),
        timestamp: EthU256::from(block.timestamp().millis() / 1_000),
        transactions,
        receipts,
    }))
}

fn receipt_envelope(
    transaction_type: u8,
    receipt: ReceiptWithBloom<AlloyReceipt>,
) -> Result<ReceiptEnvelope, RpcError> {
    match transaction_type {
        0 => Ok(ReceiptEnvelope::from_typed(TxType::Legacy, receipt)),
        1 => Ok(ReceiptEnvelope::from_typed(TxType::Eip2930, receipt)),
        2 => Ok(ReceiptEnvelope::from_typed(TxType::Eip1559, receipt)),
        4 => Ok(ReceiptEnvelope::from_typed(TxType::Eip7702, receipt)),
        other => Err(internal_error(format!(
            "unsupported EVM receipt transaction type: {other}"
        ))),
    }
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
        block_number: EthU256::from(block_number),
        transaction_hash,
        transaction_index: EthU256::from(transaction_index),
        log_index: EthU256::from(log_index),
        removed: false,
    }
}

fn alloy_log_from_evm_log(log: &evm::Log) -> AlloyLog {
    AlloyLog::new_unchecked(
        AlloyAddress::from_slice(log.address.as_ref()),
        log.topics
            .iter()
            .map(|topic| B256::from_slice(topic.as_ref()))
            .collect(),
        AlloyBytes::copy_from_slice(log.data.as_ref()),
    )
}

fn digest_to_evm_hash(digest: impl AsRef<[u8]>) -> evm::Hash {
    let mut bytes = [0u8; evm::HASH_LENGTH];
    bytes.copy_from_slice(digest.as_ref());
    evm::Hash::new(bytes)
}

pub(crate) fn evm_hash_to_block_hash(hash: evm::Hash) -> BlockHash {
    BlockHash::new(Digest::from_raw(hash.value()))
}

fn b256_to_evm_hash(hash: B256) -> evm::Hash {
    let mut bytes = [0u8; evm::HASH_LENGTH];
    bytes.copy_from_slice(hash.as_slice());
    evm::Hash::new(bytes)
}

fn bloom_hex(bloom: &Bloom) -> HexData {
    HexData::from(bloom.as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_envelope_supports_eip7702_transaction_type() {
        let envelope = receipt_envelope(4, empty_receipt()).unwrap();
        assert_eq!(envelope.tx_type(), TxType::Eip7702);
    }

    fn empty_receipt() -> ReceiptWithBloom<AlloyReceipt> {
        ReceiptWithBloom {
            receipt: AlloyReceipt {
                status: Eip658Value::Eip658(true),
                cumulative_gas_used: 0,
                logs: Vec::new(),
            },
            logs_bloom: Bloom::default(),
        }
    }
}
