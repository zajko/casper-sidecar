use std::sync::Arc;

use async_trait::async_trait;
use casper_json_rpc::{ConfigLimit, Error as RpcError, Params, RequestHandlersBuilder};

use super::{
    super::{NodeClient, RpcWithParams},
    log_filter::{LogFilter, RawLogFilter, logs_for_filter},
    projection::LogResponse,
    types::{internal_error, parse_positional_params},
};

/// `eth_getLogs`.
pub struct GetLogs;

impl GetLogs {
    pub const METHOD: &'static str = "eth_getLogs";

    pub(crate) fn register_as_handler(
        node_client: Arc<dyn NodeClient>,
        max_block_range: u64,
        handlers_builder: &mut RequestHandlersBuilder,
        limit: ConfigLimit,
    ) {
        let handler = move |maybe_params| {
            let node_client = Arc::clone(&node_client);
            async move {
                let params = Self::try_parse_params(maybe_params)?;
                Self::do_handle_request(node_client, params, max_block_range).await
            }
        };
        handlers_builder.register_handler(Self::METHOD, handler, &limit);
    }

    async fn do_handle_request(
        node_client: Arc<dyn NodeClient>,
        params: RawLogFilter,
        max_block_range: u64,
    ) -> Result<Vec<LogResponse>, RpcError> {
        logs_for_filter(node_client, &LogFilter::try_from(params)?, max_block_range).await
    }
}

#[async_trait]
impl RpcWithParams for GetLogs {
    const METHOD: &'static str = GetLogs::METHOD;
    type RequestParams = RawLogFilter;
    type ResponseResult = Vec<LogResponse>;

    fn try_parse_params(maybe_params: Option<Params>) -> Result<Self::RequestParams, RpcError> {
        let (filter,) = parse_positional_params::<(RawLogFilter,)>(maybe_params)?;
        Ok(filter)
    }

    async fn do_handle_request(
        node_client: Arc<dyn NodeClient>,
        params: RawLogFilter,
    ) -> Result<Self::ResponseResult, RpcError> {
        let _ = (node_client, params);
        Err(internal_error(
            "eth_getLogs requires configured log block range limit",
        ))
    }
}
