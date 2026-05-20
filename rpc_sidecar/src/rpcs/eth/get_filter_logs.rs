use std::sync::Arc;

use async_trait::async_trait;
use casper_json_rpc::{ConfigLimit, Error as RpcError, Params, RequestHandlersBuilder};
use casper_types::evm;

use super::{
    super::{NodeClient, RpcWithParams},
    log_filter::{EthFilterState, FilterIdParams, filter_id_from_params, logs_for_filter},
    projection::LogResponse,
    types::{internal_error, invalid_params, parse_positional_params},
};

/// `eth_getFilterLogs`.
pub struct GetFilterLogs;

impl GetFilterLogs {
    pub const METHOD: &'static str = "eth_getFilterLogs";

    pub(crate) fn register_as_handler(
        node_client: Arc<dyn NodeClient>,
        filter_state: Arc<EthFilterState>,
        max_block_range: u64,
        handlers_builder: &mut RequestHandlersBuilder,
        limit: ConfigLimit,
    ) {
        let handler = move |maybe_params| {
            let node_client = Arc::clone(&node_client);
            let filter_state = Arc::clone(&filter_state);
            async move {
                let filter_id = filter_id_from_params(maybe_params)?;
                Self::do_handle_request(node_client, filter_state, filter_id, max_block_range).await
            }
        };
        handlers_builder.register_handler(Self::METHOD, handler, &limit);
    }

    async fn do_handle_request(
        node_client: Arc<dyn NodeClient>,
        filter_state: Arc<EthFilterState>,
        filter_id: u64,
        max_block_range: u64,
    ) -> Result<Vec<LogResponse>, RpcError> {
        let filter = filter_state
            .filter(filter_id)
            .await
            .ok_or_else(|| invalid_params("filter not found"))?;
        logs_for_filter(node_client, &filter, max_block_range).await
    }
}

#[async_trait]
impl RpcWithParams for GetFilterLogs {
    const METHOD: &'static str = GetFilterLogs::METHOD;
    type RequestParams = FilterIdParams;
    type ResponseResult = Vec<LogResponse>;

    fn try_parse_params(maybe_params: Option<Params>) -> Result<Self::RequestParams, RpcError> {
        let (filter_id,) = parse_positional_params::<(evm::EthU256,)>(maybe_params)?;
        Ok(FilterIdParams { filter_id })
    }

    async fn do_handle_request(
        _node_client: Arc<dyn NodeClient>,
        _params: Self::RequestParams,
    ) -> Result<Self::ResponseResult, RpcError> {
        Err(internal_error(
            "eth_getFilterLogs requires process-local filter state",
        ))
    }
}
