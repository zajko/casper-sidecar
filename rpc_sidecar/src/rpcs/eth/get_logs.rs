use std::sync::Arc;

use async_trait::async_trait;
use casper_json_rpc::{Error as RpcError, Params};

use super::{
    super::{NodeClient, RpcWithParams},
    log_filter::{LogFilter, RawLogFilter, logs_for_filter},
    projection::LogResponse,
    types::parse_positional_params,
};

/// `eth_getLogs`.
pub struct GetLogs;

#[async_trait]
impl RpcWithParams for GetLogs {
    const METHOD: &'static str = "eth_getLogs";
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
        logs_for_filter(node_client, &LogFilter::try_from(params)?).await
    }
}
