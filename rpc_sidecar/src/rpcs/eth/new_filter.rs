use std::sync::Arc;

use async_trait::async_trait;
use casper_json_rpc::{ConfigLimit, Error as RpcError, Params, RequestHandlersBuilder};
use casper_types::evm;

use super::{
    super::{NodeClient, RpcWithParams},
    log_filter::{
        EthFilterState, LogFilter, RawLogFilter, StoredFilter, filter_id_result,
        latest_block_height,
    },
    types::{internal_error, parse_positional_params},
};

/// `eth_newFilter`.
pub struct NewFilter;

impl NewFilter {
    pub const METHOD: &'static str = "eth_newFilter";

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
                let (filter,) = parse_positional_params::<(RawLogFilter,)>(maybe_params)?;
                Self::do_handle_request(node_client, filter_state, filter).await
            }
        };
        handlers_builder.register_handler(Self::METHOD, handler, &limit);
    }

    async fn do_handle_request(
        node_client: Arc<dyn NodeClient>,
        filter_state: Arc<EthFilterState>,
        filter: RawLogFilter,
    ) -> Result<evm::EthU256, RpcError> {
        let filter = LogFilter::try_from(filter)?;
        let latest_height = if filter.block_hash().is_some() {
            None
        } else {
            latest_block_height(node_client).await?
        };
        let next_block = initial_next_block(&filter, latest_height)?;
        let filter_id = filter_state
            .insert(StoredFilter::new(filter, next_block))
            .await;
        Ok(filter_id_result(filter_id))
    }
}

fn initial_next_block(filter: &LogFilter, latest_height: Option<u64>) -> Result<u64, RpcError> {
    if filter.block_hash().is_some() {
        return Ok(0);
    }
    let Some(latest_height) = latest_height else {
        return Ok(0);
    };
    if filter.has_block_range_bound() {
        filter.from_block_height_or_latest(latest_height)
    } else {
        Ok(latest_height.saturating_add(1))
    }
}

#[async_trait]
impl RpcWithParams for NewFilter {
    const METHOD: &'static str = NewFilter::METHOD;
    type RequestParams = RawLogFilter;
    type ResponseResult = evm::EthU256;

    fn try_parse_params(maybe_params: Option<Params>) -> Result<Self::RequestParams, RpcError> {
        let (filter,) = parse_positional_params::<(RawLogFilter,)>(maybe_params)?;
        Ok(filter)
    }

    async fn do_handle_request(
        _node_client: Arc<dyn NodeClient>,
        _params: Self::RequestParams,
    ) -> Result<Self::ResponseResult, RpcError> {
        Err(internal_error(
            "eth_newFilter requires process-local filter state",
        ))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn parsed_filter(value: Value) -> LogFilter {
        let raw = serde_json::from_value::<RawLogFilter>(value).unwrap();
        LogFilter::try_from(raw).unwrap()
    }

    #[test]
    fn unbounded_filter_starts_after_latest_block() {
        let filter = parsed_filter(json!({}));

        assert_eq!(initial_next_block(&filter, Some(12)).unwrap(), 13);
    }

    #[test]
    fn unbounded_filter_without_latest_starts_at_zero() {
        let filter = parsed_filter(json!({}));

        assert_eq!(initial_next_block(&filter, None).unwrap(), 0);
    }

    #[test]
    fn bounded_filter_respects_from_block() {
        let filter = parsed_filter(json!({ "fromBlock": "earliest" }));

        assert_eq!(initial_next_block(&filter, Some(12)).unwrap(), 0);
    }
}
