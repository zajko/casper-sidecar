use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use casper_json_rpc::Error as RpcError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::super::{NodeClient, RpcWithoutParams};
use crate::{build_info, rpcs::docs::DocExample};

static CLIENT_VERSION_EXAMPLE: LazyLock<ClientVersionResult> = LazyLock::new(|| {
    ClientVersionResult(
        "CasperSidecar/v2.0.0-01234567/aarch64-unknown-linux-gnu/rustc1.91.0".to_string(),
    )
});

/// Ethereum client identity serialized as a JSON string.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
#[schemars(transparent)]
pub(crate) struct ClientVersionResult(String);

impl DocExample for ClientVersionResult {
    fn doc_example() -> &'static Self {
        &CLIENT_VERSION_EXAMPLE
    }
}

/// `web3_clientVersion`.
pub struct ClientVersion;

#[async_trait]
impl RpcWithoutParams for ClientVersion {
    const METHOD: &'static str = "web3_clientVersion";
    type ResponseResult = ClientVersionResult;

    async fn do_handle_request(
        _node_client: Arc<dyn NodeClient>,
    ) -> Result<Self::ResponseResult, RpcError> {
        Ok(ClientVersionResult(build_info::web3_client_version()))
    }
}

#[cfg(test)]
mod tests {
    use casper_json_rpc::{Params, ReservedErrorCode};
    use serde_json::{Map, Value, json};

    use super::*;
    use crate::rpcs::test_utils::BinaryPortMock;

    #[tokio::test]
    async fn returns_sidecar_build_identity_without_querying_node() {
        let result = ClientVersion::do_handle_request(Arc::new(BinaryPortMock::new()))
            .await
            .expect("client version should be available without querying the node");

        assert_eq!(result.0, build_info::web3_client_version());
        assert!(result.0.starts_with("CasperSidecar/v"));
    }

    #[test]
    fn serializes_as_json_string() {
        let result = ClientVersionResult("CasperSidecar/v2.0.0/test/rustc1.91.0".to_string());

        assert_eq!(
            serde_json::to_value(result).unwrap(),
            json!("CasperSidecar/v2.0.0/test/rustc1.91.0")
        );
    }

    #[test]
    fn accepts_absent_or_empty_params() {
        for params in [
            None,
            Some(Params::Array(Vec::new())),
            Some(Params::Object(Map::new())),
        ] {
            ClientVersion::check_no_params(params).expect("empty params should be accepted");
        }
    }

    #[test]
    fn rejects_non_empty_params() {
        let error = ClientVersion::check_no_params(Some(Params::Array(vec![Value::Null])))
            .expect_err("non-empty params should be rejected");

        assert_eq!(error.code(), ReservedErrorCode::InvalidParams as i64);
    }
}
