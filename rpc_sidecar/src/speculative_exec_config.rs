use casper_json_rpc::{ConfigLimit, nonzero_u32, nonzero_u64};
#[cfg(any(feature = "testing", test))]
use casper_json_rpc::{
    DEFAULT_LIMIT_PERIOD, DEFAULT_LIMIT_REQUESTS, DEFAULT_MAX_BATCH_ITEMS,
    DEFAULT_MAX_BATCH_RESPONSE_BYTES,
};
use casper_types::TimeDiff;
use datasize::DataSize;
use serde::Deserialize;
#[cfg(any(feature = "testing", test))]
use std::net::Ipv4Addr;
use std::{
    collections::HashMap,
    net::IpAddr,
    num::{NonZeroU32, NonZeroU64},
};

/// Default binding address for the speculative execution RPC HTTP server.
///
/// Uses a fixed port per node, but binds on any interface.
#[cfg(any(feature = "testing", test))]
const DEFAULT_IP_ADDRESS: IpAddr = IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0));
#[cfg(any(feature = "testing", test))]
const DEFAULT_PORT: u16 = 1;
/// Default rate limit in qps.
#[cfg(any(feature = "testing", test))]
const DEFAULT_QPS_LIMIT: NonZeroU32 = NonZeroU32::new(1).unwrap();
/// Default max body bytes (2.5MB).
#[cfg(any(feature = "testing", test))]
const DEFAULT_MAX_BODY_BYTES: u64 = 2_621_440;
/// Default CORS origin.
#[cfg(any(feature = "testing", test))]
const DEFAULT_CORS_ORIGIN: String = String::new();

/// JSON-RPC HTTP server configuration.
#[derive(Clone, DataSize, Debug, Deserialize)]
// Disallow unknown fields to ensure config files and command-line overrides contain valid keys.
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Setting to enable the HTTP server.
    pub enable_server: bool,
    /// IP address to bind JSON-RPC speculative execution server to.
    pub ip_address: IpAddr,
    /// Port to bind JSON-RPC speculative execution server to.
    pub port: u16,
    /// Maximum rate limit in queries per second.
    #[data_size(with = nonzero_u32)]
    pub qps_limit: NonZeroU32,
    /// Maximum number of bytes to accept in a single request body.
    pub max_body_bytes: u64,
    /// Maximum number of entries accepted in one JSON-RPC batch.
    #[data_size(with = nonzero_u32)]
    pub max_batch_items: NonZeroU32,
    /// Soft maximum size of a serialized JSON-RPC batch response.
    #[data_size(with = nonzero_u64)]
    pub max_batch_response_bytes: NonZeroU64,
    /// CORS origin.
    pub cors_origin: String,
    /// Default value for limiter's number of requests.
    #[data_size(with = nonzero_u32)]
    pub default_limit_requests: NonZeroU32,
    /// Default value for limiter's period of time.
    pub default_limit_period: TimeDiff,
    /// Limits; key is RPC method name.
    pub limits: Option<HashMap<String, ConfigLimit>>,
}

impl Config {
    pub(crate) fn default_limit(&self) -> ConfigLimit {
        ConfigLimit {
            requests: self.default_limit_requests,
            period: self.default_limit_period,
        }
    }

    #[cfg(any(feature = "testing", test))]
    pub fn test_default() -> Self {
        Config {
            enable_server: false,
            ip_address: DEFAULT_IP_ADDRESS,
            port: DEFAULT_PORT,
            qps_limit: DEFAULT_QPS_LIMIT,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            max_batch_items: DEFAULT_MAX_BATCH_ITEMS,
            max_batch_response_bytes: DEFAULT_MAX_BATCH_RESPONSE_BYTES,
            cors_origin: DEFAULT_CORS_ORIGIN,
            default_limit_requests: DEFAULT_LIMIT_REQUESTS,
            default_limit_period: DEFAULT_LIMIT_PERIOD,
            limits: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = r#"
enable_server = true
ip_address = "127.0.0.1"
port = 7778
qps_limit = 1
max_body_bytes = 2621440
max_batch_items = 8
max_batch_response_bytes = 9876
cors_origin = ""
default_limit_requests = 1
default_limit_period = "1s"
"#;

    #[test]
    fn speculative_server_config_parses_batch_limits() {
        let config: Config = toml::from_str(CONFIG).unwrap();
        assert_eq!(config.max_batch_items.get(), 8);
        assert_eq!(config.max_batch_response_bytes.get(), 9876);
    }
}
