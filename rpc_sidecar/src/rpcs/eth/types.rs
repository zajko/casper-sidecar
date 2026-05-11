use std::sync::LazyLock;

use casper_json_rpc::{Error as RpcError, Params, ReservedErrorCode};
use casper_types::{BlockIdentifier, TransactionHash, evm};
use schemars::{JsonSchema, r#gen::SchemaGenerator, schema::Schema};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use tiny_keccak::{Hasher, Keccak};

use crate::rpcs::docs::DocExample;

pub(super) const DEFAULT_ETH_CALL_GAS_LIMIT: u64 = 30_000_000;
pub(super) const ETH_LOG_BLOOM_LENGTH: usize = 256;

static ETH_U256_EXAMPLE: LazyLock<evm::EthU256> = LazyLock::new(|| evm::EthU256::ZERO);
static ETH_ADDRESS_EXAMPLE: LazyLock<EthAddress> = LazyLock::new(|| EthAddress(evm::Address::ZERO));
static EVM_HASH_EXAMPLE: LazyLock<evm::Hash> = LazyLock::new(|| evm::Hash::ZERO);
static HEX_DATA_EXAMPLE: LazyLock<HexData> = LazyLock::new(|| HexData(Vec::new()));

impl DocExample for evm::EthU256 {
    fn doc_example() -> &'static Self {
        &ETH_U256_EXAMPLE
    }
}

/// Ethereum address encoded as fixed-width `0x` hexadecimal.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub(crate) struct EthAddress(evm::Address);

impl EthAddress {
    pub(super) fn into_inner(self) -> evm::Address {
        self.0
    }
}

impl From<evm::Address> for EthAddress {
    fn from(address: evm::Address) -> Self {
        EthAddress(address)
    }
}

impl From<EthAddress> for String {
    fn from(value: EthAddress) -> Self {
        format!("0x{}", value.0.to_hex_string())
    }
}

impl TryFrom<String> for EthAddress {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        parse_address(&value).map(EthAddress)
    }
}

impl JsonSchema for EthAddress {
    fn schema_name() -> String {
        "EthAddress".to_string()
    }

    fn json_schema(schema_generator: &mut SchemaGenerator) -> Schema {
        String::json_schema(schema_generator)
    }
}

impl DocExample for EthAddress {
    fn doc_example() -> &'static Self {
        &ETH_ADDRESS_EXAMPLE
    }
}

impl DocExample for evm::Hash {
    fn doc_example() -> &'static Self {
        &EVM_HASH_EXAMPLE
    }
}

/// Variable-width byte data encoded as `0x` hexadecimal.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub(crate) struct HexData(Vec<u8>);

impl HexData {
    pub(super) fn into_bytes(self) -> Vec<u8> {
        self.0
    }
}

impl From<Vec<u8>> for HexData {
    fn from(bytes: Vec<u8>) -> Self {
        HexData(bytes)
    }
}

impl From<&[u8]> for HexData {
    fn from(bytes: &[u8]) -> Self {
        HexData(bytes.to_vec())
    }
}

impl From<HexData> for String {
    fn from(value: HexData) -> Self {
        bytes_hex(&value.0)
    }
}

impl TryFrom<String> for HexData {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        parse_hex_bytes(&value).map(HexData)
    }
}

impl JsonSchema for HexData {
    fn schema_name() -> String {
        "EthHexData".to_string()
    }

    fn json_schema(schema_generator: &mut SchemaGenerator) -> Schema {
        String::json_schema(schema_generator)
    }
}

impl DocExample for HexData {
    fn doc_example() -> &'static Self {
        &HEX_DATA_EXAMPLE
    }
}

/// Ethereum block tags supported by this first-pass RPC surface.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum BlockTag {
    /// Latest finalized Casper state known to the node.
    Latest,
    /// Alias for latest until pending EVM state is exposed.
    Pending,
}

impl Default for BlockTag {
    fn default() -> Self {
        BlockTag::Latest
    }
}

/// Block selector accepted by `eth_getBlockByNumber`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub(crate) enum BlockNumberParam {
    /// Latest block known by the node.
    Tag(BlockTag),
    /// Concrete block height.
    Height(evm::EthU256),
}

impl BlockNumberParam {
    pub(super) fn identifier(self) -> Result<Option<BlockIdentifier>, RpcError> {
        match self {
            BlockNumberParam::Tag(BlockTag::Latest | BlockTag::Pending) => Ok(None),
            BlockNumberParam::Height(height) => height
                .as_u64()
                .map(|height| Some(BlockIdentifier::Height(height)))
                .map_err(invalid_params),
        }
    }
}

pub(super) fn parse_positional_params<T>(maybe_params: Option<Params>) -> Result<T, RpcError>
where
    T: DeserializeOwned,
{
    let params = match maybe_params {
        Some(params) => Value::from(params),
        None => {
            return Err(RpcError::new(
                ReservedErrorCode::InvalidParams,
                "Missing 'params' field",
            ));
        }
    };
    serde_json::from_value(params).map_err(|error| {
        RpcError::new(
            ReservedErrorCode::InvalidParams,
            format!("Failed to parse 'params' field: {error}"),
        )
    })
}

pub(super) fn transaction_hash_to_evm_hash(transaction_hash: TransactionHash) -> Option<evm::Hash> {
    match transaction_hash {
        TransactionHash::Evm(hash) => Some(hash.hash()),
        TransactionHash::Deploy(_) | TransactionHash::V1(_) => None,
    }
}

pub(super) fn digest_to_evm_hash(digest: impl AsRef<[u8]>) -> evm::Hash {
    let mut bytes = [0u8; evm::HASH_LENGTH];
    bytes.copy_from_slice(digest.as_ref());
    evm::Hash::new(bytes)
}

pub(super) fn block_hash_to_evm_hash(block_hash: impl AsRef<[u8]>) -> evm::Hash {
    let mut bytes = [0u8; evm::HASH_LENGTH];
    bytes.copy_from_slice(block_hash.as_ref());
    evm::Hash::new(bytes)
}

pub(super) fn logs_bloom(logs: &[evm::Log]) -> Vec<u8> {
    let mut bloom = [0u8; ETH_LOG_BLOOM_LENGTH];
    for log in logs {
        add_bloom_entry(&mut bloom, log.address.as_ref());
        for topic in &log.topics {
            add_bloom_entry(&mut bloom, topic.as_ref());
        }
    }
    bloom.to_vec()
}

/// Implements the Ethereum Yellow Paper, section 4.4.1, equations (30)-(34):
/// `M3:2048` sets `B2047-m(x,i)` for `i` in `{0, 2, 4}`, where
/// `m(x, i) = KEC(x)[i, i + 1] mod 2048`.
fn add_bloom_entry(bloom: &mut [u8; ETH_LOG_BLOOM_LENGTH], bytes: &[u8]) {
    let hash = keccak(bytes);
    for index in [0usize, 2, 4] {
        let bit = ((((hash[index] as usize) << 8) | hash[index + 1] as usize) & 2047) as usize;
        bloom[ETH_LOG_BLOOM_LENGTH - 1 - bit / 8] |= 1 << (bit % 8);
    }
}

fn keccak(bytes: &[u8]) -> [u8; evm::HASH_LENGTH] {
    let mut hasher = Keccak::v256();
    let mut output = [0u8; evm::HASH_LENGTH];
    hasher.update(bytes);
    hasher.finalize(&mut output);
    output
}

fn parse_address(value: &str) -> Result<evm::Address, String> {
    let bytes = parse_fixed_hex::<{ evm::ADDRESS_LENGTH }>(value)?;
    Ok(evm::Address::new(bytes))
}

fn parse_fixed_hex<const N: usize>(value: &str) -> Result<[u8; N], String> {
    let bytes = parse_hex_bytes(value)?;
    <[u8; N]>::try_from(bytes.as_slice())
        .map_err(|_| format!("expected {N} bytes, got {}", bytes.len()))
}

fn parse_hex_bytes(value: &str) -> Result<Vec<u8>, String> {
    let hex = value
        .strip_prefix("0x")
        .ok_or_else(|| "hex value must start with 0x".to_string())?;
    if hex.len() % 2 != 0 {
        return Err("hex value must have an even number of digits".to_string());
    }
    (0..hex.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&hex[index..index + 2], 16)
                .map_err(|error| format!("invalid hex: {error}"))
        })
        .collect()
}

pub(super) fn invalid_params(message: impl Into<String>) -> RpcError {
    RpcError::new(ReservedErrorCode::InvalidParams, message.into())
}

pub(super) fn internal_error(message: impl ToString) -> RpcError {
    RpcError::new(ReservedErrorCode::InternalError, message.to_string())
}

fn bytes_hex(bytes: &[u8]) -> String {
    format!("0x{}", base16::encode_lower(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evm_hash_uses_json_rpc_hex_prefix() {
        let hash = evm::Hash::new([0xab; evm::HASH_LENGTH]);
        let expected_hex = "ab".repeat(evm::HASH_LENGTH);

        assert_eq!(format!("{hash}"), format!("0x{expected_hex}"));

        let encoded = serde_json::to_string(&hash).expect("evm hash should serialize");
        assert_eq!(encoded, format!("\"0x{expected_hex}\""));
        assert_eq!(
            serde_json::from_str::<evm::Hash>(&encoded).expect("evm hash should parse"),
            hash
        );
    }
}
