use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use casper_json_rpc::Error as RpcError;
use casper_types::BlockSynchronizerStatus;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    super::{NodeClient, RpcWithoutParams},
    eth_u256::EthU256,
    types::internal_error,
};
use crate::rpcs::docs::DocExample;

static SYNCING_RESULT_EXAMPLE: LazyLock<SyncingResult> =
    LazyLock::new(|| SyncingResult::NotSyncing(false));

/// Reactor states in which the node is still catching up to the tip of the chain, as opposed to
/// having caught up and either following or validating the tip.
fn is_catching_up(reactor_state: &str) -> bool {
    matches!(reactor_state, "Initialize" | "CatchUp")
}

fn highest_known_block_height(block_sync: &BlockSynchronizerStatus, fallback: u64) -> u64 {
    #[derive(Default, Deserialize)]
    struct BlockSyncStatusMirror {
        #[serde(default)]
        block_height: Option<u64>,
    }

    #[derive(Default, Deserialize)]
    struct BlockSynchronizerStatusMirror {
        #[serde(default)]
        forward: Option<BlockSyncStatusMirror>,
    }

    let mirror: BlockSynchronizerStatusMirror = serde_json::to_value(block_sync)
        .ok()
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();

    mirror
        .forward
        .and_then(|status| status.block_height)
        .unwrap_or(fallback)
}

/// Sync progress reported while the node has not yet caught up to the tip of the chain.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SyncingStatus {
    /// Height of the lowest block held contiguously by this node.
    starting_block: EthU256,
    /// Height of the highest block held contiguously by this node.
    current_block: EthU256,
    /// Height of the highest block this node is aware of, whether or not it holds it yet.
    highest_block: EthU256,
}

/// `eth_syncing` result: `false` once the node has caught up to tip, otherwise sync progress.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub(crate) enum SyncingResult {
    NotSyncing(bool),
    Syncing(SyncingStatus),
}

impl DocExample for SyncingResult {
    fn doc_example() -> &'static Self {
        &SYNCING_RESULT_EXAMPLE
    }
}

/// `eth_syncing`.
pub struct Syncing;

#[async_trait]
impl RpcWithoutParams for Syncing {
    const METHOD: &'static str = "eth_syncing";
    type ResponseResult = SyncingResult;

    async fn do_handle_request(
        node_client: Arc<dyn NodeClient>,
    ) -> Result<Self::ResponseResult, RpcError> {
        let status = node_client
            .read_node_status()
            .await
            .map_err(internal_error)?;

        if !is_catching_up(&status.reactor_state.into_inner()) {
            return Ok(SyncingResult::NotSyncing(false));
        }

        let starting_block = status.available_block_range.low();
        let current_block = status.available_block_range.high();
        let highest_block =
            highest_known_block_height(&status.block_sync, current_block).max(current_block);

        Ok(SyncingResult::Syncing(SyncingStatus {
            starting_block: EthU256::from(starting_block),
            current_block: EthU256::from(current_block),
            highest_block: EthU256::from(highest_block),
        }))
    }
}

#[cfg(test)]
mod tests {
    use casper_binary_port::{
        BinaryResponse, Command, InformationRequest, NodeStatus, ReactorStateName,
    };
    use casper_types::{
        AvailableBlockRange, BlockSynchronizerStatus, Peers, ProtocolVersion, TimeDiff, Timestamp,
        testing::TestRng,
    };
    use serde_json::json;

    use super::*;
    use crate::rpcs::test_utils::BinaryPortMock;

    fn node_status(reactor_state: &str, block_sync: BlockSynchronizerStatus) -> NodeStatus {
        let rng = &mut TestRng::new();
        NodeStatus {
            protocol_version: ProtocolVersion::from_parts(2, 0, 0),
            peers: Peers::random(rng),
            build_version: "test".to_string(),
            chainspec_name: "test-net".to_string(),
            starting_state_root_hash: Default::default(),
            last_added_block_info: None,
            our_public_signing_key: None,
            round_length: None,
            next_upgrade: None,
            uptime: TimeDiff::from_seconds(1),
            reactor_state: ReactorStateName::new(reactor_state),
            last_progress: Timestamp::now(),
            available_block_range: AvailableBlockRange::new(5, 100),
            block_sync,
            latest_switch_block_hash: None,
        }
    }

    async fn client_with_status(status: NodeStatus) -> BinaryPortMock {
        let client = BinaryPortMock::new();
        let request = InformationRequest::NodeStatus
            .try_into()
            .expect("node status information request should convert");
        client
            .when_then(Command::Get(request), BinaryResponse::from_value(status))
            .await;
        client
    }

    #[tokio::test]
    async fn reports_false_once_caught_up() {
        let client = client_with_status(node_status(
            "KeepUp",
            BlockSynchronizerStatus::new(None, None),
        ))
        .await;

        let result = Syncing::do_handle_request(Arc::new(client))
            .await
            .expect("syncing lookup should succeed");

        assert_eq!(result, SyncingResult::NotSyncing(false));
        assert_eq!(serde_json::to_value(result).unwrap(), json!(false));
    }

    #[tokio::test]
    async fn reports_progress_while_catching_up() {
        let block_sync = BlockSynchronizerStatus::new(
            None,
            Some(casper_types::BlockSyncStatus::new(
                Default::default(),
                Some(250),
                "have block header(250)".to_string(),
            )),
        );
        let client = client_with_status(node_status("CatchUp", block_sync)).await;

        let result = Syncing::do_handle_request(Arc::new(client))
            .await
            .expect("syncing lookup should succeed");

        assert_eq!(
            result,
            SyncingResult::Syncing(SyncingStatus {
                starting_block: EthU256::from(5u64),
                current_block: EthU256::from(100u64),
                highest_block: EthU256::from(250u64),
            })
        );
    }

    #[tokio::test]
    async fn falls_back_to_current_block_when_sync_target_unknown() {
        let client = client_with_status(node_status(
            "Initialize",
            BlockSynchronizerStatus::new(None, None),
        ))
        .await;

        let result = Syncing::do_handle_request(Arc::new(client))
            .await
            .expect("syncing lookup should succeed");

        assert_eq!(
            result,
            SyncingResult::Syncing(SyncingStatus {
                starting_block: EthU256::from(5u64),
                current_block: EthU256::from(100u64),
                highest_block: EthU256::from(100u64),
            })
        );
    }
}
