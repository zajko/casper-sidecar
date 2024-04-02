use std::{str, sync::Arc};

use async_trait::async_trait;
use once_cell::sync::Lazy;
use rand::Rng;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use casper_types::{account::AccountHash, contracts::{ContractHash, ContractPackage, ContractPackageStatus, ContractVersionKey, ContractVersions, DisabledVersions}, testing::TestRng, Deploy, DeployHash, Groups, StoredValue, Transaction, TransactionHash, URef};

use super::{
    docs::{DocExample, DOCS_EXAMPLE_API_VERSION},
    ApiVersion, ClientError, Error, NodeClient, RpcError, RpcWithParams, RpcWithoutParams,
    CURRENT_API_VERSION,
};

#[derive(Clone, PartialEq, Serialize, Deserialize, JsonSchema, Debug)]
#[serde(deny_unknown_fields)]
pub struct XResult {
    value: StoredValue,
}

static X_RPCS_RESULT: Lazy<XResult> = Lazy::new(|| {
    let p = StoredValue::CLValue(casper_types::CLValue::from_t(42u32).unwrap());
    XResult { value: p }
});

impl DocExample for XResult {
    fn doc_example() -> &'static Self {
        &X_RPCS_RESULT
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Debug)]
pub struct X {}

#[async_trait]
impl RpcWithoutParams for X {
    const METHOD: &'static str = "x";
    type ResponseResult = XResult;

    async fn do_handle_request(
        _node_client: Arc<dyn NodeClient>,
    ) -> Result<Self::ResponseResult, RpcError> {
        let mut rng = TestRng::new();
        
        let access_key = URef::default();
        let mut versions = ContractVersions::default();
        let account_hash = AccountHash::new(rng.gen());
        let contract_hash = ContractHash::new(account_hash.0);
        versions.insert(ContractVersionKey::new(50, 120), contract_hash);


        let disabled_versions = DisabledVersions::default();
        let groups = Groups::default();
        let lock_status = ContractPackageStatus::default();
        let mut package = ContractPackage::new(
            access_key, versions, disabled_versions, groups, lock_status,  
        );
        let a = StoredValue::ContractPackage(package);
        Ok(XResult { value: a })
    }
}
