# Changelog

All notable changes to this project will be documented in this file. The format is based on [Keep a Changelog].

[comment]: <> (Added: new features)
[comment]: <> (Changed: changes in existing functionality)
[comment]: <> (Deprecated: soon-to-be removed features)
[comment]: <> (Removed: now removed features)
[comment]: <> (Fixed: any bug fixes)
[comment]: <> (Security: in case of vulnerabilities)

## [3.0.0] -

### Added

- JSON RPC API has a new action "state_query_bids".
- DNS name resolution for RPC server node host:
  -- `rpc_server.node_client.ip_address` configuration can now be a DNS hostname (if `enable_dns_resolution` is set to `true`). Also, this field will be removed in a future release and will be replaced by it's alias `host`.
  -- `rpc_server.node_client.host` is an alias for `ip_address`. This is the future go-to configuration name.
  -- `rpc_server.node_client.enable_dns_resolution` if set to true, then `ip_address`/`host` will be resolved as dns name
- DNS name resolution for SSE connections:
  -- `sse_server.connections.ip_address` configuration can now be a DNS hostname (if `enable_dns_resolution` is set to `true`). Also, this field will be removed in a future release and will be replaced by it's alias `host`.
  -- `sse_server.connections.host` is an alias for `ip_address`. This is the future go-to configuration name.
  -- `sse_server.connections.enable_dns_resolution` if set to true, then `ip_address`/`host` will be resolved as dns name
- DNS name resolution for SSE connections:

### Changed

- `TransactionRuntimeParams::VmCasperV2::seed` now serializes/deserializes as `String`

## [2.0.0] -

### Added

- `account_put_transaction` now handles `TransactionInvocationTarget::ByPackageHash` with `protocol_version_major`
- `account_put_transaction` now handles `TransactionInvocationTarget::ByPackageName` with `protocol_version_major`
- Compatible with `casper-types` in 6.0.1 version

## [1.0.4] -

### Added

- Initial release of node for Sidecar.
