use async_trait::async_trait;
use hickory_resolver::{
    Resolver, name_server::GenericConnector, proto::runtime::TokioRuntimeProvider,
};
use std::{
    net::{IpAddr, SocketAddr},
    str::FromStr,
    sync::OnceLock,
};
use thiserror::Error;
use tracing::error;

use crate::NodeClientConfig;

#[cfg_attr(test, derive(PartialEq))]
#[derive(Debug, Error)]
pub enum SocketResolutionError {
    #[error("Could not initialize DNS resolver. Details: {0}")]
    DnsResolverInitialization(String),
    #[error("Could not resolve config entry {0} as dns name")]
    CouldNotResolveAsDns(String),
    #[error("Could not resolve config entry {0} as ip address")]
    CouldNotResolveAsIp(String),
}

#[async_trait]
pub trait SocketResolver: Sync + Send {
    async fn resolve_socket(&self) -> Result<SocketAddr, SocketResolutionError>;
}

pub fn build_socket_resolver(
    node_client_config: &NodeClientConfig,
) -> Box<dyn SocketResolver + 'static> {
    let resolve_dns = node_client_config.enable_dns_resolution.unwrap_or(false);
    let host = &node_client_config.ip_address;
    let port = node_client_config.port;
    if resolve_dns {
        Box::new(DnsEnabledSocketResolver::new(host, port))
    } else {
        Box::new(DnsDisabledSocketResolver::new(host, port))
    }
}

#[cfg(test)]
pub fn test_socket_resolver(ip_address: &str, port: u16) -> Box<dyn SocketResolver + 'static> {
    Box::new(DnsDisabledSocketResolver::new(ip_address, port))
}

static RESOLVER: OnceLock<Resolver<GenericConnector<TokioRuntimeProvider>>> = OnceLock::new();

struct DnsEnabledSocketResolver {
    host: String,
    port: u16,
}

#[async_trait]
impl SocketResolver for DnsEnabledSocketResolver {
    async fn resolve_socket(&self) -> Result<SocketAddr, SocketResolutionError> {
        let dns_name_or_ip_address = &self.host;
        let port = self.port;
        let resolver = match RESOLVER.get() {
            Some(resolver) => resolver,
            None => {
                let resolver = Resolver::builder_tokio()
                    .map_err(|err| {
                        SocketResolutionError::DnsResolverInitialization(format!("{err}"))
                    })?
                    .build();
                let _ = RESOLVER.set(resolver); //We ignore the result since it would be Err only if RESOLVER is already set
                RESOLVER.get().unwrap() // unwrapping is set because we ensure that the resolver inst empty in the line before
            }
        };
        let res = resolver.lookup_ip(dns_name_or_ip_address).await;
        match res {
            Ok(lookup_ip) => match lookup_ip.into_iter().next() {
                Some(ip_addr) => Ok(SocketAddr::new(ip_addr, port)),
                None => Err(SocketResolutionError::CouldNotResolveAsDns(
                    dns_name_or_ip_address.to_owned(),
                )),
            },
            Err(e) => {
                error!("Error when resolving ip address for {dns_name_or_ip_address}. Reason: {e}");
                Err(SocketResolutionError::CouldNotResolveAsDns(
                    dns_name_or_ip_address.to_owned(),
                ))
            }
        }
    }
}

impl DnsEnabledSocketResolver {
    pub fn new(dns_name_or_ip_address: &str, port: u16) -> Self {
        Self {
            host: dns_name_or_ip_address.to_owned(),
            port,
        }
    }
}

struct DnsDisabledSocketResolver {
    ip_address: String,
    port: u16,
}

impl DnsDisabledSocketResolver {
    pub fn new(ip_address: &str, port: u16) -> Self {
        Self {
            ip_address: ip_address.to_owned(),
            port,
        }
    }
}

#[async_trait]
impl SocketResolver for DnsDisabledSocketResolver {
    async fn resolve_socket(&self) -> Result<SocketAddr, SocketResolutionError> {
        let ip_address = &self.ip_address;
        let port = self.port;
        let ip_address = IpAddr::from_str(ip_address).map_err(|err| {
            error!("Couldn't parse as ip address: {ip_address}. Reason: {err}");
            SocketResolutionError::CouldNotResolveAsIp(ip_address.to_owned())
        })?;
        Ok(SocketAddr::new(ip_address, port))
    }
}

#[cfg(test)]
mod tests {
    use crate::node_client::socket_resolver::{
        DnsDisabledSocketResolver, DnsEnabledSocketResolver, SocketResolutionError, SocketResolver,
    };

    #[tokio::test]
    async fn dns_enabled_socket_resolver_should_resolve_google() {
        let under_test = DnsEnabledSocketResolver::new("www.google.com", 1111);
        let res = under_test.resolve_socket().await;
        assert!(res.is_ok());
        assert_eq!(res.unwrap().port(), 1111);
    }

    #[tokio::test]
    async fn dns_disabled_socket_resolver_should_not_resolve_google() {
        let under_test = DnsDisabledSocketResolver::new("www.google.com", 1111);
        let res = under_test.resolve_socket().await;
        assert!(res.is_err());
        assert_eq!(
            res.err().unwrap(),
            SocketResolutionError::CouldNotResolveAsIp("www.google.com".to_owned())
        );
    }

    #[tokio::test]
    async fn dns_disabled_socket_resolver_should_resolve_ip_address() {
        let under_test = DnsDisabledSocketResolver::new("155.91.122.5", 1111);
        let res = under_test.resolve_socket().await;
        assert!(res.is_ok());
        let socket = res.unwrap();
        assert_eq!(socket.port(), 1111);
        assert_eq!(socket.ip().to_string(), "155.91.122.5".to_owned());
    }
}
