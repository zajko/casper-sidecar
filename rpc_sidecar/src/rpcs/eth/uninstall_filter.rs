use std::sync::Arc;

use async_trait::async_trait;
use casper_json_rpc::{ConfigLimit, Error as RpcError, Params, RequestHandlersBuilder};
use casper_types::evm;

use super::{
    super::{NodeClient, RpcWithParams},
    log_filter::{EthFilterState, FilterIdParams, filter_id_from_params},
    types::{internal_error, parse_positional_params},
};

/// `eth_uninstallFilter`.
pub struct UninstallFilter;

impl UninstallFilter {
    pub const METHOD: &'static str = "eth_uninstallFilter";

    pub(crate) fn register_as_handler(
        filter_state: Arc<EthFilterState>,
        handlers_builder: &mut RequestHandlersBuilder,
        limit: ConfigLimit,
    ) {
        let handler = move |maybe_params| {
            let filter_state = Arc::clone(&filter_state);
            async move {
                let filter_id = filter_id_from_params(maybe_params)?;
                Ok(filter_state.remove(filter_id).await)
            }
        };
        handlers_builder.register_handler(Self::METHOD, handler, &limit);
    }
}

#[async_trait]
impl RpcWithParams for UninstallFilter {
    const METHOD: &'static str = UninstallFilter::METHOD;
    type RequestParams = FilterIdParams;
    type ResponseResult = bool;

    fn try_parse_params(maybe_params: Option<Params>) -> Result<Self::RequestParams, RpcError> {
        let (filter_id,) = parse_positional_params::<(evm::EthU256,)>(maybe_params)?;
        Ok(FilterIdParams { filter_id })
    }

    async fn do_handle_request(
        _node_client: Arc<dyn NodeClient>,
        _params: Self::RequestParams,
    ) -> Result<Self::ResponseResult, RpcError> {
        Err(internal_error(
            "eth_uninstallFilter requires process-local filter state",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpcs::eth::log_filter::{LogFilter, RawLogFilter, StoredFilter};

    #[tokio::test]
    async fn filter_state_uninstall_reports_whether_filter_existed() {
        let state = EthFilterState::new();
        let filter_id = state
            .insert(StoredFilter::new(
                LogFilter::try_from(RawLogFilter::default()).unwrap(),
                0,
            ))
            .await;

        assert!(state.remove(filter_id).await);
        assert!(!state.remove(filter_id).await);
    }
}
