# The `casper-json-rpc` Library

[![LOGO](https://raw.githubusercontent.com/casper-network/casper-node/master/images/casper-association-logo-primary.svg)](https://casper.network/)

[![Crates.io](https://img.shields.io/crates/v/casper-json-rpc)](https://crates.io/crates/casper-json-rpc)
[![Documentation](https://docs.rs/casper-node/badge.svg)](https://docs.rs/casper-json-rpc)
[![License](https://img.shields.io/badge/license-Apache-blue)](https://github.com/casper-network/casper-node/blob/master/LICENSE)

The `casper-json-rpc` library described here can be used as the framework for a JSON-RPC server.

# Usage

Typical usage of this library involves two steps:

* Construct a set of request handlers using a
[`RequestHandlersBuilder`](https://docs.rs/casper-json-rpc/latest/casper_json_rpc/struct.RequestHandlersBuilder.html).
* Call [`casper_json_rpc::route`](https://docs.rs/casper-json-rpc/latest/casper_json_rpc/fn.route.html) to construct a boxed warp filter ready to be passed to [`warp::service`](https://docs.rs/warp/latest/warp/fn.service.html).

# Example

```rust
use casper_json_rpc::{ConfigLimit, Error, JsonRpcOptions, Params, RequestHandlersBuilder};
use std::{convert::Infallible};

async fn get(params: Option<Params>) -> Result<String, Error> {
    // * parse params or return `ReservedErrorCode::InvalidParams` error
    // * handle request and return result
    Ok("got it".to_string())
}

async fn put(params: Option<Params>, other_input: &str) -> Result<String, Error> {
    Ok(other_input.to_string())
}

#[tokio::main]
async fn main() {
    // Register handlers for methods "get" and "put".
    let mut handlers = RequestHandlersBuilder::new();
    let limit = ConfigLimit::default();
    handlers.register_handler("get", get, &limit);
    let put_handler = move |params| async move { put(params, "other input").await };
    handlers.register_handler("put", put_handler, &limit);
    let handlers = handlers.build();

    // Get the new route.
    let path = "rpc";
    let max_body_bytes = 1024;
    let options = JsonRpcOptions::default();
    let route = casper_json_rpc::route(path, max_body_bytes, handlers, options);

    // Convert it into a `Service` and run it.
    let make_svc = hyper::service::make_service_fn(move |_| {
        let svc = warp::service(route.clone());
        async move { Ok::<_, Infallible>(svc.clone()) }
    });

    hyper::Server::bind(&([127, 0, 0, 1], 3030).into())
        .serve(make_svc)
        .await
        .unwrap();
}
```

The following is a sample request:

```sh
curl -X POST -H 'Content-Type: application/json' -d '{"jsonrpc":"2.0","id":"id","method":"get"}' http://127.0.0.1:3030/rpc
```

Here is a sample response:

```sh
{"jsonrpc":"2.0","id":"id","result":"got it"}
```

## Batches, notifications, and custom transports

Version 3 accepts JSON-RPC 2.0 batches and notifications. A notification is a fully valid request
whose `id` field is absent; its handler still runs, but no success or error response is emitted.
An explicit `"id": null` is a call and does receive a response. Batch responses preserve request
order after notification entries are omitted.

For non-HTTP transports, implement `RequestDispatcher` and call
`handle_json_request_bytes`. It returns `JsonRpcOutput::NoResponse`, `Single`, or a non-empty
`Batch`. HTTP maps `NoResponse` to `204 No Content`; WebSocket transports should send no frame.
Use `Notification::new` with serializable parameters when a server transport needs to emit an
outbound JSON-RPC notification.

`JsonRpcOptions` controls unknown-field validation, the maximum batch length, and the soft maximum
serialized batch-response size. Its defaults are 100 entries and 25,000,000 response bytes.

### Version 3 wire migration

`params: null` is no longer accepted. Omit `params` when a method takes no parameters, or send an
empty array (`"params": []`) when a client requires the field. Array and object parameters remain
supported.

# Errors

To return a JSON-RPC response indicating an error, use
[`Error::new`](https://docs.rs/casper-json-rpc/latest/casper_json_rpc/struct.Error.html#method.new).  Most error
conditions that require returning a reserved error are already handled in the provided warp filters.  The only
exception is
[`ReservedErrorCode::InvalidParams`](https://docs.rs/casper-json-rpc/latest/casper_json_rpc/enum.ReservedErrorCode.html#variant.InvalidParams), which should be returned by any RPC handler that deems the provided `params: Option<Params>` to be invalid for any
reason.

Generally, a set of custom error codes should be provided.  These should all implement
[`ErrorCodeT`](https://docs.rs/casper-json-rpc/latest/casper_json_rpc/trait.ErrorCodeT.html).

## Example custom error code

```rust
use serde::{Deserialize, Serialize};
use casper_json_rpc::ErrorCodeT;

#[derive(Copy, Clone, Eq, PartialEq, Serialize, Deserialize, Debug)]
#[repr(i64)]
pub enum ErrorCode {
    /// The requested item was not found.
    NoSuchItem = -1,
    /// Failed to put the requested item to storage.
    FailedToPutItem = -2,
}

impl From<ErrorCode> for (i64, &'static str) {
    fn from(error_code: ErrorCode) -> Self {
        match error_code {
            ErrorCode::NoSuchItem => (error_code as i64, "No such item"),
            ErrorCode::FailedToPutItem => (error_code as i64, "Failed to put item"),
        }
    }
}

impl ErrorCodeT for ErrorCode {}
```

# License

Licensed under the [Apache License Version 2.0](https://github.com/casper-network/casper-node/blob/master/LICENSE).
