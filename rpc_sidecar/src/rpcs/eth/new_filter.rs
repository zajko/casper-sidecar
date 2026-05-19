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
        let next_block = if filter.block_hash().is_some() {
            0
        } else {
            let latest_height = match latest_block_height(node_client).await? {
                Some(latest_height) => latest_height,
                None => 0,
            };
            filter.from_block_height_or_latest(latest_height)?
        };
        let filter_id = filter_state
            .insert(StoredFilter::new(filter, next_block))
            .await;
        Ok(filter_id_result(filter_id))
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
