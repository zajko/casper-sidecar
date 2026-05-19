use std::sync::Arc;

use async_trait::async_trait;
use casper_json_rpc::{ConfigLimit, Error as RpcError, Params, RequestHandlersBuilder};

use super::{
    super::{NodeClient, RpcWithParams},
    log_filter::{
        EthFilterState, FilterIdParams, filter_id_from_params, latest_block_height,
        logs_for_block_range, logs_for_filter,
    },
    projection::LogResponse,
    types::{internal_error, invalid_params, parse_positional_params},
};

/// `eth_getFilterChanges`.
pub struct GetFilterChanges;

impl GetFilterChanges {
    pub const METHOD: &'static str = "eth_getFilterChanges";

    pub(crate) fn register_as_handler(
        node_client: Arc<dyn NodeClient>,
        filter_state: Arc<EthFilterState>,
        handlers_builder: &mut RequestHandlersBuilder,
        limit: ConfigLimit,
    ) {
        let handler = move |maybe_params| {
            let node_client = Arc::clone(&node_client);
            let filter_state = Arc::clone(&filter_state);
            async move {
                let filter_id = filter_id_from_params(maybe_params)?;
                Self::do_handle_request(node_client, filter_state, filter_id).await
            }
        };
        handlers_builder.register_handler(Self::METHOD, handler, &limit);
    }

    async fn do_handle_request(
        node_client: Arc<dyn NodeClient>,
        filter_state: Arc<EthFilterState>,
        filter_id: u64,
    ) -> Result<Vec<LogResponse>, RpcError> {
        let stored = filter_state
            .get(filter_id)
            .await
            .ok_or_else(|| invalid_params("filter not found"))?;

        if stored.filter.block_hash().is_some() {
            if stored.block_hash_polled {
                return Ok(Vec::new());
            }
            let logs = logs_for_filter(node_client, &stored.filter).await?;
            filter_state.mark_block_hash_polled(filter_id).await;
            return Ok(logs);
        }

        let Some(latest_height) = latest_block_height(node_client.clone()).await? else {
            return Ok(Vec::new());
        };
        let to_height = stored
            .filter
            .to_block_height(latest_height)?
            .min(latest_height);
        let Some(next_block) = next_block_after_filter_changes_poll(stored.next_block, to_height)
        else {
            return Ok(Vec::new());
        };
        let logs =
            logs_for_block_range(node_client, &stored.filter, stored.next_block, to_height).await?;
        filter_state.set_next_block(filter_id, next_block).await;
        Ok(logs)
    }
}

fn next_block_after_filter_changes_poll(next_block: u64, to_height: u64) -> Option<u64> {
    if next_block > to_height {
        None
    } else {
        Some(to_height.saturating_add(1))
    }
}

#[async_trait]
impl RpcWithParams for GetFilterChanges {
    const METHOD: &'static str = GetFilterChanges::METHOD;
    type RequestParams = FilterIdParams;
    type ResponseResult = Vec<LogResponse>;

    fn try_parse_params(maybe_params: Option<Params>) -> Result<Self::RequestParams, RpcError> {
        let (filter_id,) = parse_positional_params::<(casper_types::evm::EthU256,)>(maybe_params)?;
        Ok(FilterIdParams { filter_id })
    }

    async fn do_handle_request(
        _node_client: Arc<dyn NodeClient>,
        _params: Self::RequestParams,
    ) -> Result<Self::ResponseResult, RpcError> {
        Err(internal_error(
            "eth_getFilterChanges requires process-local filter state",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_changes_cursor_does_not_rewind_future_filters() {
        assert_eq!(next_block_after_filter_changes_poll(100, 50), None);
        assert_eq!(next_block_after_filter_changes_poll(50, 50), Some(51));
        assert_eq!(
            next_block_after_filter_changes_poll(u64::MAX, u64::MAX),
            Some(u64::MAX)
        );
    }
}
