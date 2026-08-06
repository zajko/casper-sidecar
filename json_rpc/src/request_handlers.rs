use std::{collections::HashMap, future::Future, sync::Arc, time::Instant};

use futures::{FutureExt, future::BoxFuture};
use governor::{
    DefaultDirectRateLimiter,
    clock::{Clock, DefaultClock},
};
use metrics::rpc::{inc_method_call, observe_response_time, register_request_size};
use serde::Serialize;
use serde_json::Value;
use tracing::{debug, error};

use crate::{
    ConfigLimit,
    error::{Error, ReservedErrorCode, RpcErrorCode},
    request::Params,
};

/// A request-handling closure.
type RequestHandler =
    Arc<dyn Fn(Option<Params>) -> BoxFuture<'static, Result<Value, Error>> + Send + Sync>;

/// Dispatches one validated JSON-RPC request.
///
/// The envelope processor owns request IDs and notification response suppression. Implementations
/// are responsible for method lookup, rate limiting, metrics, and invoking the method handler.
/// Dispatcher futures are awaited inline and are not required to be [`Send`].
#[allow(async_fn_in_trait)]
pub trait RequestDispatcher {
    /// Dispatches `method` with the validated optional `params` value.
    async fn dispatch(
        &mut self,
        method: &str,
        params: Option<Params>,
        request_size: usize,
    ) -> Result<Value, Error>;
}

/// A clonable per-method rate limiter suitable for custom dispatchers.
#[derive(Clone)]
pub struct MethodLimiter(Arc<DefaultDirectRateLimiter>);

impl MethodLimiter {
    /// Creates a rate limiter from `limit`.
    #[must_use]
    pub fn new(limit: &ConfigLimit) -> Self {
        Self(Arc::new(DefaultDirectRateLimiter::direct(limit.quota())))
    }

    /// Checks the limit and returns the standard JSON-RPC throttling error when exhausted.
    pub fn check(&self) -> Result<(), Error> {
        self.0.check().map_err(|negative| {
            let wait_time = negative.wait_time_from(DefaultClock::default().now());
            Error::new(
                RpcErrorCode::RequestThrottled,
                format!("retry-after {:.4}s", wait_time.as_secs_f32()),
            )
        })
    }
}

/// A collection of request-handlers, indexed by the JSON-RPC "method" applicable to each.
///
/// There needs to be a unique handler for each JSON-RPC request "method" to be handled.  Handlers
/// are added via a [`RequestHandlersBuilder`].
#[derive(Clone)]
pub struct RequestHandlers(Arc<HashMap<&'static str, (RequestHandler, MethodLimiter)>>);

impl RequestDispatcher for RequestHandlers {
    async fn dispatch(
        &mut self,
        request_method: &str,
        params: Option<Params>,
        request_size: usize,
    ) -> Result<Value, Error> {
        let start = Instant::now();
        let entry = self
            .0
            .get(request_method)
            .map(|(handler, limiter)| (Arc::clone(handler), limiter.clone()));
        let Some((handler, limiter)) = entry else {
            observe_response_time("unknown-handler", "unknown-handler", start.elapsed());
            debug!(requested_method = %request_method, "failed to get handler");
            return Err(Error::new(
                ReservedErrorCode::MethodNotFound,
                format!("'{request_method}' is not a supported json-rpc method on this server"),
            ));
        };
        inc_method_call(request_method);
        register_request_size(request_method, request_size);
        if let Err(error) = limiter.check() {
            observe_response_time(request_method, &error.code().to_string(), start.elapsed());
            return Err(error);
        }

        match handler(params).await {
            Ok(result) => {
                observe_response_time(request_method, "success", start.elapsed());
                Ok(result)
            }
            Err(error) => {
                observe_response_time(request_method, &error.code().to_string(), start.elapsed());
                Err(error)
            }
        }
    }
}

/// A builder for [`RequestHandlers`].
//
// This builder exists so the internal `HashMap` can be populated before it is made immutable behind
// the `Arc` in the `RequestHandlers`.
#[derive(Default)]
pub struct RequestHandlersBuilder(HashMap<&'static str, (RequestHandler, MethodLimiter)>);

impl RequestHandlersBuilder {
    /// Returns a new builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a new request-handler which will be called to handle all JSON-RPC requests with the
    /// given "method" field.
    ///
    /// The handler should be an async closure or function with a signature like:
    /// ```ignore
    /// async fn handle_it(params: Option<Params>) -> Result<T, Error>
    /// ```
    /// where `T` implements `Serialize` and will be used as the JSON-RPC response's "result" field.
    pub fn register_handler<Func, Fut, T>(
        &mut self,
        method: &'static str,
        handler: Func,
        limit: &ConfigLimit,
    ) where
        Func: Fn(Option<Params>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<T, Error>> + Send,
        T: Serialize + 'static,
    {
        // The provided handler returns a future with output of `Result<T, Error>`. We need to
        // convert that to a boxed future with output `Result<Value, Error>` to store it in a
        // homogenous collection.
        let handler = Arc::new(handler);
        let wrapped_handler = move |maybe_params| {
            let handler = Arc::clone(&handler);
            async move {
                let success = handler(maybe_params).await?;
                serde_json::to_value(success).map_err(|error| {
                    error!(%error, "failed to encode json-rpc response value");
                    Error::new(
                        ReservedErrorCode::InternalError,
                        format!("failed to encode json-rpc response value: {error}"),
                    )
                })
            }
            .boxed()
        };
        if self
            .0
            .insert(
                method,
                (Arc::new(wrapped_handler), MethodLimiter::new(limit)),
            )
            .is_some()
        {
            error!(
                method,
                "already registered a handler for this json-rpc request method"
            );
        }
    }

    /// Finalize building by converting `self` to a [`RequestHandlers`].
    #[must_use]
    pub fn build(self) -> RequestHandlers {
        RequestHandlers(Arc::new(self.0))
    }
}
