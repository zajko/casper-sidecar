use std::sync::{Arc, LazyLock};

use async_trait::async_trait;
use casper_json_rpc::{Error as RpcError, Params, ReservedErrorCode};
use casper_types::{
    EvmConfig, EvmTransaction, InitiatorAddr, TimeDiff, Timestamp, U256,
    account::{ACCOUNT_HASH_LENGTH, AccountHash},
    evm,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{
    super::{NodeClient, RpcWithParams},
    eth_u256::EthU256,
    types::{
        BlockTag, DEFAULT_ETH_CALL_GAS_LIMIT, EthAddress, HexData, internal_error, invalid_params,
        parse_positional_params,
    },
};
use crate::rpcs::docs::DocExample;

static CALL_PARAMS_EXAMPLE: LazyLock<CallParams> = LazyLock::new(|| CallParams {
    call: CallObject {
        from: Some(EthAddress::from(evm::Address::ZERO)),
        to: None,
        data: Some(HexData::from(Vec::new())),
        input: None,
        value: None,
        gas: None,
        gas_price: None,
    },
    block: BlockTag::Latest,
});

const DEFAULT_EVM_CALL_TTL: TimeDiff = TimeDiff::from_seconds(300);

#[derive(Deserialize)]
struct ChainspecEvmConfig {
    evm: EvmConfig,
}

/// Call object accepted by `eth_call`.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CallObject {
    from: Option<EthAddress>,
    to: Option<EthAddress>,
    data: Option<HexData>,
    input: Option<HexData>,
    value: Option<EthU256>,
    gas: Option<EthU256>,
    gas_price: Option<EthU256>,
}

impl CallObject {
    fn from(&self) -> evm::Address {
        self.from
            .map(EthAddress::into_inner)
            .unwrap_or(evm::Address::ZERO)
    }

    fn to(&self) -> Option<evm::Address> {
        self.to.map(EthAddress::into_inner)
    }

    fn input(&self) -> Result<Vec<u8>, RpcError> {
        match (&self.data, &self.input) {
            (Some(data), Some(input)) if data != input => Err(invalid_params(
                "eth_call data and input fields must match when both are supplied",
            )),
            (Some(data), _) => Ok(data.clone().into_bytes()),
            (_, Some(input)) => Ok(input.clone().into_bytes()),
            (None, None) => Ok(Vec::new()),
        }
    }

    fn value(&self) -> U256 {
        self.value
            .as_ref()
            .map(|value| value.value())
            .unwrap_or_else(U256::zero)
    }

    fn gas_limit(&self) -> Result<u64, RpcError> {
        self.gas
            .map(EthU256::as_u64)
            .transpose()
            .map_err(invalid_params)
            .map(|maybe_gas| maybe_gas.unwrap_or(DEFAULT_ETH_CALL_GAS_LIMIT))
    }

    fn gas_price(&self, default_base_fee: u64) -> Result<u128, RpcError> {
        self.gas_price
            .map(eth_u256_to_u128)
            .transpose()
            .map(|maybe_gas_price| maybe_gas_price.unwrap_or(u128::from(default_base_fee)))
    }
}

/// Params for `eth_call`.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CallParams {
    call: CallObject,
    #[serde(default)]
    block: BlockTag,
}

impl CallParams {
    fn call(&self) -> &CallObject {
        &self.call
    }
}

impl DocExample for CallParams {
    fn doc_example() -> &'static Self {
        &CALL_PARAMS_EXAMPLE
    }
}

#[derive(Deserialize)]
struct PositionalParams(CallObject, #[serde(default)] BlockTag);

impl From<PositionalParams> for CallParams {
    fn from(params: PositionalParams) -> Self {
        CallParams {
            call: params.0,
            block: params.1,
        }
    }
}

fn eth_u256_to_u128(value: EthU256) -> Result<u128, RpcError> {
    let value = value.value();
    if value > U256::from(u128::MAX) {
        return Err(invalid_params("quantity exceeds u128"));
    }
    let mut bytes = [0u8; 32];
    value.to_big_endian(&mut bytes);
    Ok(u128::from_be_bytes(
        bytes[16..]
            .try_into()
            .expect("slice should contain exactly sixteen bytes"),
    ))
}

async fn read_evm_config(node_client: &dyn NodeClient) -> Result<EvmConfig, RpcError> {
    let chainspec = node_client
        .read_chainspec_bytes()
        .await
        .map_err(internal_error)?;
    let chainspec = std::str::from_utf8(chainspec.chainspec_bytes())
        .map_err(|error| internal_error(format!("invalid chainspec bytes: {error}")))?;
    let chainspec = toml::from_str::<ChainspecEvmConfig>(chainspec)
        .map_err(|error| internal_error(format!("invalid chainspec toml: {error}")))?;
    Ok(chainspec.evm)
}

fn new_evm_call_transaction(
    call: &CallObject,
    evm_config: EvmConfig,
) -> Result<EvmTransaction, RpcError> {
    let from = call.from();
    let initiator_addr = evm_account_hash_initiator(from);
    Ok(EvmTransaction::new_unsigned_call(
        Timestamp::zero(),
        DEFAULT_EVM_CALL_TTL,
        initiator_addr,
        evm_config.chain_id,
        from,
        call.to(),
        call.value(),
        call.input()?,
        call.gas_limit()?,
        call.gas_price(evm_config.base_fee)?,
    ))
}

fn evm_account_hash_initiator(from: evm::Address) -> InitiatorAddr {
    let mut account_hash = [0; ACCOUNT_HASH_LENGTH];
    // `eth_call` has no Casper public key, but node-side EVM execution now
    // requires an initiator. Use a deterministic AccountHash representation
    // with the 20-byte EVM address in the low bytes and zero padding above it.
    account_hash[ACCOUNT_HASH_LENGTH - evm::ADDRESS_LENGTH..].copy_from_slice(from.as_bytes());
    InitiatorAddr::AccountHash(AccountHash::new(account_hash))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EthCallError {
    message: String,
    data: HexData,
    gas_used: EthU256,
}

/// `eth_call`.
pub struct Call;

#[async_trait]
impl RpcWithParams for Call {
    const METHOD: &'static str = "eth_call";
    type RequestParams = CallParams;
    type ResponseResult = HexData;

    fn try_parse_params(maybe_params: Option<Params>) -> Result<Self::RequestParams, RpcError> {
        parse_positional_params::<PositionalParams>(maybe_params).map(Into::into)
    }

    async fn do_handle_request(
        node_client: Arc<dyn NodeClient>,
        params: CallParams,
    ) -> Result<HexData, RpcError> {
        let call = params.call();
        let evm_config = read_evm_config(node_client.as_ref()).await?;
        let transaction = new_evm_call_transaction(call, evm_config)?;
        let result = node_client
            .evm_call(transaction)
            .await
            .map_err(internal_error)?;
        let receipt = result.evm_receipt();
        if receipt.status.is_success() {
            Ok(HexData::from(result.evm_output()))
        } else {
            Err(RpcError::new(
                ReservedErrorCode::InternalError,
                EthCallError {
                    message: receipt
                        .status
                        .message()
                        .unwrap_or("EVM call failed")
                        .to_string(),
                    data: HexData::from(result.evm_output()),
                    gas_used: EthU256::from(receipt.gas_used),
                },
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVM_CHAIN_ID: u64 = 7;
    const EVM_BASE_FEE: u64 = 1;

    #[test]
    fn eth_call_builds_unsigned_evm_transaction() {
        let from = evm::Address::new([1; evm::ADDRESS_LENGTH]);
        let to = evm::Address::new([2; evm::ADDRESS_LENGTH]);
        let input = vec![0xde, 0xad];

        let transaction = new_evm_call_transaction(
            &CallObject {
                from: Some(EthAddress::from(from)),
                to: Some(EthAddress::from(to)),
                data: Some(HexData::from(input.clone())),
                input: None,
                value: Some(EthU256::from(U256::from(1))),
                gas: Some(EthU256::from(1_000u64)),
                gas_price: None,
            },
            evm_config(),
        )
        .expect("transaction should build");

        assert!(transaction.is_unsigned_call());
        assert!(transaction.approval().is_none());
        assert_eq!(transaction.chain_id(), Some(EVM_CHAIN_ID));
        assert_eq!(transaction.from(), from);
        assert_eq!(
            transaction.initiator_addr(),
            &evm_account_hash_initiator(from)
        );
        assert_eq!(transaction.to(), Some(to));
        assert_eq!(transaction.value(), U256::from(1));
        assert_eq!(transaction.input(), input.as_slice());
        assert_eq!(transaction.gas_limit(), 1_000);
        assert_eq!(transaction.gas_price(), Some(u128::from(EVM_BASE_FEE)));
    }

    #[test]
    fn eth_call_uses_explicit_gas_price() {
        let gas_price = u128::from(EVM_BASE_FEE) + 1;

        let transaction = new_evm_call_transaction(
            &CallObject {
                gas_price: Some(EthU256::from(gas_price)),
                ..CallObject::default()
            },
            evm_config(),
        )
        .expect("transaction should build");

        assert_eq!(transaction.gas_price(), Some(gas_price));
    }

    #[test]
    fn eth_u256_to_u128_rejects_overflow() {
        let result = eth_u256_to_u128(EthU256::from(U256::from(u128::MAX) + U256::one()));

        assert!(matches!(
            result,
            Err(error) if error.code() == ReservedErrorCode::InvalidParams as i64
        ));
    }

    fn evm_config() -> EvmConfig {
        EvmConfig {
            enabled: true,
            chain_id: EVM_CHAIN_ID,
            base_fee: EVM_BASE_FEE,
            ..Default::default()
        }
    }
}
