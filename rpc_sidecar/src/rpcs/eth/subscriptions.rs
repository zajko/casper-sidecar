use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use casper_event_types::SidecarEvent;
use casper_json_rpc::{
    CorsOrigin, Error as RpcError, RequestHandlers, ReservedErrorCode, Response,
    handle_json_request_bytes,
};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{
    sync::{
        broadcast::{Receiver as BroadcastReceiver, Sender as BroadcastSender, error::RecvError},
        mpsc::{self, UnboundedSender},
    },
    task::JoinHandle,
    time::sleep,
};
use tracing::{debug, info, warn};
use warp::{
    Filter, Rejection, Reply,
    filters::BoxedFilter,
    http::StatusCode,
    reject::Reject,
    reply,
    ws::{Message, WebSocket},
};

use super::{
    log_filter::{
        LogFilter, RawLogFilter, ensure_log_block_range_within_limit, latest_block_height,
        logs_for_block_height, logs_for_filter,
    },
    types::invalid_params,
};
use crate::rpcs::NodeClient;

const LOGS_SUBSCRIPTION: &str = "logs";
const LOG_FETCH_RETRY_ATTEMPTS: usize = 10;
const LOG_FETCH_RETRY_DELAY: Duration = Duration::from_secs(1);
const LOG_BACKFILL_BLOCK_DELAY: Duration = Duration::from_millis(25);

static NEXT_SUBSCRIPTION_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) fn websocket_route(
    api_path: &'static str,
    handlers: RequestHandlers,
    node_client: Arc<dyn NodeClient>,
    sidecar_event_sender: BroadcastSender<SidecarEvent>,
    allow_unknown_fields: bool,
    origin_policy: WebSocketOriginPolicy,
    max_eth_log_block_range: u64,
) -> BoxedFilter<(reply::Response,)> {
    warp::path::path(api_path)
        .and(warp::path::end())
        .and(warp::ws())
        .and(websocket_origin_filter(origin_policy))
        .map(move |ws: warp::ws::Ws| {
            let handlers = handlers.clone();
            let node_client = node_client.clone();
            let sidecar_event_sender = sidecar_event_sender.clone();
            ws.on_upgrade(move |websocket| {
                handle_websocket(
                    websocket,
                    handlers,
                    node_client,
                    sidecar_event_sender,
                    allow_unknown_fields,
                    max_eth_log_block_range,
                )
            })
            .into_response()
        })
        .recover(handle_websocket_rejection)
        .unify()
        .boxed()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WebSocketOriginPolicy {
    NoBrowserOrigins,
    Any,
    Specified(String),
}

impl WebSocketOriginPolicy {
    pub(crate) fn from_cors_header(cors_header: Option<&CorsOrigin>) -> Self {
        match cors_header {
            Some(CorsOrigin::Any) => Self::Any,
            Some(CorsOrigin::Specified(origin)) => Self::Specified(origin.clone()),
            None => Self::NoBrowserOrigins,
        }
    }
}

#[derive(Debug)]
struct WebSocketOriginForbidden;

impl Reject for WebSocketOriginForbidden {}

fn websocket_origin_filter(policy: WebSocketOriginPolicy) -> BoxedFilter<()> {
    warp::header::optional::<String>("origin")
        .and_then(move |origin: Option<String>| {
            let policy = policy.clone();
            async move {
                if websocket_origin_allowed(&policy, origin.as_deref()) {
                    Ok(())
                } else {
                    Err(warp::reject::custom(WebSocketOriginForbidden))
                }
            }
        })
        .untuple_one()
        .boxed()
}

fn websocket_origin_allowed(policy: &WebSocketOriginPolicy, origin: Option<&str>) -> bool {
    match policy {
        WebSocketOriginPolicy::NoBrowserOrigins => origin.is_none(),
        WebSocketOriginPolicy::Any => true,
        WebSocketOriginPolicy::Specified(allowed_origin) => {
            origin.is_some_and(|origin| origin == allowed_origin)
        }
    }
}

async fn handle_websocket_rejection(error: Rejection) -> Result<reply::Response, Rejection> {
    if error.find::<WebSocketOriginForbidden>().is_some() {
        Ok(reply::with_status("Forbidden", StatusCode::FORBIDDEN).into_response())
    } else {
        Err(error)
    }
}

async fn handle_websocket(
    websocket: WebSocket,
    handlers: RequestHandlers,
    node_client: Arc<dyn NodeClient>,
    sidecar_event_sender: BroadcastSender<SidecarEvent>,
    allow_unknown_fields: bool,
    max_eth_log_block_range: u64,
) {
    info!("eth websocket connection opened");
    let (mut websocket_tx, mut websocket_rx) = websocket.split();
    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<Value>();
    let writer = tokio::spawn(async move {
        while let Some(value) = outbound_rx.recv().await {
            if websocket_tx
                .send(Message::text(value.to_string()))
                .await
                .is_err()
            {
                warn!("eth websocket writer failed; closing writer task");
                break;
            }
        }
    });

    let mut subscriptions: HashMap<String, JoinHandle<()>> = HashMap::new();
    while let Some(message) = websocket_rx.next().await {
        let Ok(message) = message else {
            break;
        };
        if message.is_close() {
            break;
        }
        if !(message.is_text() || message.is_binary()) {
            continue;
        }

        let body = message.as_bytes();
        let method = serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|value| {
                value
                    .get("method")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });

        match method.as_deref() {
            Some("eth_subscribe") => {
                handle_subscribe(
                    body,
                    node_client.clone(),
                    sidecar_event_sender.clone(),
                    &outbound_tx,
                    &mut subscriptions,
                    max_eth_log_block_range,
                )
                .await;
            }
            Some("eth_unsubscribe") => {
                handle_unsubscribe(body, &outbound_tx, &mut subscriptions);
            }
            _ => {
                if let Some(response) =
                    handle_json_request_bytes(body, handlers.clone(), allow_unknown_fields).await
                {
                    send_response(&outbound_tx, response);
                }
            }
        }
    }

    for (_, subscription) in subscriptions {
        subscription.abort();
    }
    writer.abort();
    info!("eth websocket connection closed");
}

async fn handle_subscribe(
    body: &[u8],
    node_client: Arc<dyn NodeClient>,
    sidecar_event_sender: BroadcastSender<SidecarEvent>,
    outbound_tx: &UnboundedSender<Value>,
    subscriptions: &mut HashMap<String, JoinHandle<()>>,
    max_block_range: u64,
) {
    let request = match serde_json::from_slice::<Value>(body) {
        Ok(request) => request,
        Err(error) => {
            send_response(
                outbound_tx,
                Response::new_failure(
                    Value::Null,
                    RpcError::new(ReservedErrorCode::ParseError, error.to_string()),
                ),
            );
            return;
        }
    };
    let id = request_id(&request);
    let filter = match parse_subscribe_filter(&request) {
        Ok(filter) => filter,
        Err(error) => {
            send_response(outbound_tx, Response::new_failure(id, error));
            return;
        }
    };
    let subscription_start =
        match prepare_log_subscription_start(node_client.clone(), &filter, max_block_range).await {
            Ok(subscription_start) => subscription_start,
            Err(error) => {
                send_response(outbound_tx, Response::new_failure(id, error));
                return;
            }
        };

    let subscription_id = next_subscription_id();
    send_response(
        outbound_tx,
        Response::new_success(id, json!(subscription_id.clone())),
    );

    let notification_tx = outbound_tx.clone();
    let subscription_node_client = node_client.clone();
    let subscription_filter = filter.clone();
    let subscription_id_for_task = subscription_id.clone();
    let sidecar_event_receiver = sidecar_event_sender.subscribe();
    info!(
        subscription_id,
        filter = ?filter,
        active_subscriptions = subscriptions.len() + 1,
        sidecar_event_receivers = sidecar_event_sender.receiver_count(),
        "created eth logs subscription"
    );
    let handle = tokio::spawn(async move {
        run_log_subscription(
            subscription_node_client,
            subscription_filter,
            subscription_id_for_task,
            sidecar_event_receiver,
            notification_tx,
            subscription_start,
            max_block_range,
        )
        .await;
    });
    subscriptions.insert(subscription_id.clone(), handle);
}

fn handle_unsubscribe(
    body: &[u8],
    outbound_tx: &UnboundedSender<Value>,
    subscriptions: &mut HashMap<String, JoinHandle<()>>,
) {
    let request = match serde_json::from_slice::<Value>(body) {
        Ok(request) => request,
        Err(error) => {
            send_response(
                outbound_tx,
                Response::new_failure(
                    Value::Null,
                    RpcError::new(ReservedErrorCode::ParseError, error.to_string()),
                ),
            );
            return;
        }
    };
    let id = request_id(&request);
    let subscription_id = match parse_unsubscribe_id(&request) {
        Ok(subscription_id) => subscription_id,
        Err(error) => {
            send_response(outbound_tx, Response::new_failure(id, error));
            return;
        }
    };
    let existed = subscriptions
        .remove(&subscription_id)
        .map(|handle| {
            handle.abort();
            true
        })
        .unwrap_or(false);
    info!(subscription_id, existed, "eth unsubscribe requested");
    send_response(outbound_tx, Response::new_success(id, json!(existed)));
}

#[derive(Clone, Copy, Debug)]
struct LogSubscriptionStart {
    latest_height: Option<u64>,
    finite_to_block: Option<u64>,
    initial_range: Option<(u64, u64)>,
    next_height: u64,
}

async fn prepare_log_subscription_start(
    node_client: Arc<dyn NodeClient>,
    filter: &LogFilter,
    max_block_range: u64,
) -> Result<Option<LogSubscriptionStart>, RpcError> {
    if filter.block_hash().is_some() {
        return Ok(None);
    }

    let latest_height = latest_block_height(node_client).await?;
    let finite_to_block = filter.finite_to_block_height()?;
    let (initial_range, next_height) = initial_subscription_range(filter, latest_height)?;
    if let Some((from_height, to_height)) = initial_range {
        ensure_log_block_range_within_limit(from_height, to_height, max_block_range)?;
    }

    Ok(Some(LogSubscriptionStart {
        latest_height,
        finite_to_block,
        initial_range,
        next_height,
    }))
}

async fn run_log_subscription(
    node_client: Arc<dyn NodeClient>,
    filter: LogFilter,
    subscription_id: String,
    mut sidecar_event_receiver: BroadcastReceiver<SidecarEvent>,
    outbound_tx: UnboundedSender<Value>,
    subscription_start: Option<LogSubscriptionStart>,
    max_block_range: u64,
) {
    if filter.block_hash().is_some() {
        info!(
            subscription_id,
            filter = ?filter,
            "starting block-hash eth logs subscription"
        );
        match logs_for_filter_with_retries(node_client, &filter, max_block_range).await {
            Ok(logs) => {
                info!(
                    subscription_id,
                    log_count = logs.len(),
                    "fetched block-hash eth logs subscription"
                );
                for log in logs {
                    send_notification(&outbound_tx, &subscription_id, log);
                }
            }
            Err(error) => {
                warn!(?error, "failed to fetch logs for block-hash subscription");
            }
        }
        futures::future::pending::<()>().await;
        return;
    }

    let Some(subscription_start) = subscription_start else {
        warn!(
            subscription_id,
            "missing prepared eth logs subscription start"
        );
        return;
    };
    let latest_height = subscription_start.latest_height;
    let finite_to_block = subscription_start.finite_to_block;
    let initial_range = subscription_start.initial_range;
    let mut next_height = subscription_start.next_height;
    info!(
        subscription_id,
        latest_height,
        finite_to_block,
        initial_range = ?initial_range,
        next_height,
        filter = ?filter,
        "starting eth logs subscription task"
    );

    if let Some((from_height, to_height)) = initial_range {
        match send_logs_for_block_range_with_retries(
            node_client.clone(),
            &filter,
            from_height,
            to_height,
            &subscription_id,
            &outbound_tx,
            max_block_range,
        )
        .await
        {
            Ok(outcome) => {
                info!(
                    subscription_id,
                    from_height,
                    to_height,
                    emitted_logs = outcome.emitted_logs,
                    next_height = outcome.next_height,
                    "finished initial eth logs subscription backfill"
                );
                next_height = outcome.next_height;
            }
            Err(error) => {
                warn!(
                    error = ?error.error,
                    from_height, to_height, "failed to fetch initial log subscription range"
                );
                next_height = error.next_height;
                if error.fatal {
                    return;
                }
            }
        }
    }

    if subscription_is_complete(next_height, finite_to_block) {
        info!(
            subscription_id,
            next_height, finite_to_block, "eth logs subscription completed after initial range"
        );
        return;
    }

    loop {
        tokio::select! {
            event = sidecar_event_receiver.recv() => {
                match event {
                    Ok(SidecarEvent::BlockAdded { height, .. }) => {
                        info!(
                            subscription_id,
                            event_height = height,
                            next_height,
                            "eth logs subscription received BlockAdded event"
                        );
                        let Some((from_height, to_height, next_after_event)) =
                            block_range_after_event(next_height, height, finite_to_block)
                        else {
                            debug!(
                                subscription_id,
                                event_height = height,
                                next_height,
                                finite_to_block,
                                "eth logs subscription skipped old BlockAdded event"
                            );
                            continue;
                        };
                        match send_logs_for_block_range_with_retries(
                            node_client.clone(),
                            &filter,
                            from_height,
                            to_height,
                            &subscription_id,
                            &outbound_tx,
                            max_block_range,
                        )
                        .await
                        {
                            Ok(outcome) => {
                                debug_assert_eq!(outcome.next_height, next_after_event);
                                info!(
                                    subscription_id,
                                    from_height,
                                    to_height,
                                    emitted_logs = outcome.emitted_logs,
                                    next_height = outcome.next_height,
                                    "processed eth logs subscription BlockAdded range"
                                );
                                next_height = outcome.next_height;
                            }
                            Err(error) => {
                                warn!(
                                    error = ?error.error,
                                    from_height, to_height, "failed to fetch logs for new block range"
                                );
                                next_height = error.next_height;
                                if error.fatal {
                                    return;
                                }
                            }
                        }
                    }
                    Err(RecvError::Lagged(_)) => {
                        warn!(
                            subscription_id,
                            next_height,
                            "eth logs subscription lagged on sidecar event receiver"
                        );
                        let Ok(Some(latest_height)) = latest_block_height(node_client.clone()).await else {
                            warn!(
                                subscription_id,
                                "failed to read latest block height after lagged sidecar event receiver"
                            );
                            continue;
                        };
                        info!(
                            subscription_id,
                            latest_height,
                            next_height,
                            "eth logs subscription recovering from lagged sidecar event receiver"
                        );
                        let Some((from_height, to_height, next_after_event)) =
                            block_range_after_event(next_height, latest_height, finite_to_block)
                        else {
                            debug!(
                                subscription_id,
                                latest_height,
                                next_height,
                                finite_to_block,
                                "eth logs subscription lag recovery found no new range"
                            );
                            continue;
                        };
                        match send_logs_for_block_range_with_retries(
                            node_client.clone(),
                            &filter,
                            from_height,
                            to_height,
                            &subscription_id,
                            &outbound_tx,
                            max_block_range,
                        )
                        .await
                        {
                            Ok(outcome) => {
                                debug_assert_eq!(outcome.next_height, next_after_event);
                                info!(
                                    subscription_id,
                                    from_height,
                                    to_height,
                                    emitted_logs = outcome.emitted_logs,
                                    next_height = outcome.next_height,
                                    "processed eth logs subscription lag recovery range"
                                );
                                next_height = outcome.next_height;
                            }
                            Err(error) => {
                                warn!(
                                    error = ?error.error,
                                    from_height, to_height, "failed to catch up lagged log subscription"
                                );
                                next_height = error.next_height;
                                if error.fatal {
                                    return;
                                }
                            }
                        }
                    }
                    Err(RecvError::Closed) => {
                        warn!(
                            subscription_id,
                            "eth logs subscription sidecar event channel closed"
                        );
                        break;
                    }
                }
            }
        }

        if subscription_is_complete(next_height, finite_to_block) {
            info!(
                subscription_id,
                next_height, finite_to_block, "eth logs subscription completed"
            );
            return;
        }
    }
}

fn initial_subscription_range(
    filter: &LogFilter,
    latest_height: Option<u64>,
) -> Result<(Option<(u64, u64)>, u64), RpcError> {
    let Some(latest_height) = latest_height else {
        return Ok((None, 0));
    };

    if !filter.has_block_range_bound() {
        return Ok((None, latest_height.saturating_add(1)));
    }

    let from_height = filter.from_block_height_or_latest(latest_height)?;
    let to_height = filter.to_block_height(latest_height)?.min(latest_height);
    if from_height > to_height {
        Ok((None, from_height))
    } else {
        Ok((Some((from_height, to_height)), to_height.saturating_add(1)))
    }
}

fn block_range_after_event(
    next_height: u64,
    event_height: u64,
    finite_to_block: Option<u64>,
) -> Option<(u64, u64, u64)> {
    let to_height = finite_to_block
        .map(|max_height| event_height.min(max_height))
        .unwrap_or(event_height);
    if to_height < next_height {
        None
    } else {
        Some((next_height, to_height, to_height.saturating_add(1)))
    }
}

fn subscription_is_complete(next_height: u64, finite_to_block: Option<u64>) -> bool {
    finite_to_block.is_some_and(|to_block| next_height > to_block)
}

#[derive(Debug)]
struct LogRangeFetchError {
    error: RpcError,
    next_height: u64,
    fatal: bool,
}

#[derive(Debug)]
struct LogRangeFetchOutcome {
    next_height: u64,
    emitted_logs: usize,
}

async fn logs_for_filter_with_retries(
    node_client: Arc<dyn NodeClient>,
    filter: &LogFilter,
    max_block_range: u64,
) -> Result<Vec<super::projection::LogResponse>, RpcError> {
    let mut attempts_remaining = LOG_FETCH_RETRY_ATTEMPTS;
    loop {
        match logs_for_filter(node_client.clone(), filter, max_block_range).await {
            Ok(logs) => return Ok(logs),
            Err(error) if attempts_remaining > 0 => {
                attempts_remaining -= 1;
                warn!(
                    ?error,
                    attempts_remaining, "retrying log subscription filter fetch"
                );
                sleep(LOG_FETCH_RETRY_DELAY).await;
            }
            Err(error) => return Err(error),
        }
    }
}

async fn send_logs_for_block_range_with_retries(
    node_client: Arc<dyn NodeClient>,
    filter: &LogFilter,
    from_height: u64,
    to_height: u64,
    subscription_id: &str,
    outbound_tx: &UnboundedSender<Value>,
    max_block_range: u64,
) -> Result<LogRangeFetchOutcome, LogRangeFetchError> {
    if let Err(error) = ensure_log_block_range_within_limit(from_height, to_height, max_block_range)
    {
        return Err(LogRangeFetchError {
            error,
            next_height: from_height,
            fatal: true,
        });
    }

    let mut next_height = from_height;
    let mut emitted_logs = 0usize;
    for height in from_height..=to_height {
        let logs =
            match logs_for_block_height_with_retries(node_client.clone(), filter, height).await {
                Ok(logs) => logs,
                Err(error) => {
                    return Err(LogRangeFetchError {
                        error,
                        next_height,
                        fatal: false,
                    });
                }
            };
        let log_count = logs.len();
        if log_count > 0 {
            info!(
                subscription_id,
                height, log_count, "eth logs subscription matched block logs"
            );
        } else {
            debug!(
                subscription_id,
                height, "eth logs subscription found no matching logs in block"
            );
        }
        for log in logs {
            send_notification(outbound_tx, subscription_id, log);
        }
        emitted_logs = emitted_logs.saturating_add(log_count);
        next_height = height.saturating_add(1);
        if height < to_height {
            // Backfills can touch many blocks. Pace them so a ranged
            // `eth_subscribe` request does not exhaust the node binary-port
            // one-second request window before live block events arrive.
            sleep(LOG_BACKFILL_BLOCK_DELAY).await;
        }
    }
    Ok(LogRangeFetchOutcome {
        next_height,
        emitted_logs,
    })
}

async fn logs_for_block_height_with_retries(
    node_client: Arc<dyn NodeClient>,
    filter: &LogFilter,
    height: u64,
) -> Result<Vec<super::projection::LogResponse>, RpcError> {
    let mut attempts_remaining = LOG_FETCH_RETRY_ATTEMPTS;
    loop {
        match logs_for_block_height(node_client.clone(), filter, height).await {
            Ok(logs) => return Ok(logs),
            Err(error) if attempts_remaining > 0 => {
                attempts_remaining -= 1;
                warn!(
                    ?error,
                    attempts_remaining, height, "retrying log subscription block fetch"
                );
                sleep(LOG_FETCH_RETRY_DELAY).await;
            }
            Err(error) => return Err(error),
        }
    }
}

fn parse_subscribe_filter(request: &Value) -> Result<LogFilter, RpcError> {
    let params = request
        .get("params")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_params("'params' must be an array"))?;
    let subscription_kind = params
        .first()
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_params("missing subscription type"))?;
    if subscription_kind != LOGS_SUBSCRIPTION {
        return Err(invalid_params(format!(
            "unsupported subscription type: {subscription_kind}"
        )));
    }
    if params.len() > 2 {
        return Err(invalid_params(
            "'eth_subscribe' accepts a subscription type and optional filter object",
        ));
    }
    let raw_filter = match params.get(1) {
        Some(filter) => {
            serde_json::from_value::<RawLogFilter>(filter.clone()).map_err(|error| {
                invalid_params(format!("failed to parse log subscription filter: {error}"))
            })?
        }
        None => RawLogFilter::default(),
    };
    LogFilter::try_from(raw_filter)
}

fn parse_unsubscribe_id(request: &Value) -> Result<String, RpcError> {
    let params = request
        .get("params")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_params("'params' must be an array"))?;
    if params.len() != 1 {
        return Err(invalid_params(
            "'eth_unsubscribe' accepts exactly one subscription id",
        ));
    }
    params[0]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| invalid_params("subscription id must be a string"))
}

fn request_id(request: &Value) -> Value {
    request.get("id").cloned().unwrap_or(Value::Null)
}

fn send_response(outbound_tx: &UnboundedSender<Value>, response: Response) {
    if let Ok(value) = serde_json::to_value(response) {
        let _ = outbound_tx.send(value);
    }
}

fn send_notification(
    outbound_tx: &UnboundedSender<Value>,
    subscription_id: &str,
    log: super::projection::LogResponse,
) {
    debug!(
        subscription_id,
        block_number = %log.block_number,
        transaction_hash = %log.transaction_hash,
        log_index = %log.log_index,
        "sending eth subscription log notification"
    );
    if outbound_tx
        .send(json!({
            "jsonrpc": "2.0",
            "method": "eth_subscription",
            "params": {
                "subscription": subscription_id,
                "result": log,
            },
        }))
        .is_err()
    {
        warn!(
            subscription_id,
            "failed to enqueue eth subscription notification; websocket writer is closed"
        );
    }
}

fn next_subscription_id() -> String {
    let id = NEXT_SUBSCRIPTION_ID.fetch_add(1, Ordering::Relaxed);
    format!("0x{id:x}")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn parses_log_subscription_filter() {
        let filter = parse_subscribe_filter(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_subscribe",
            "params": ["logs", { "topics": [null] }]
        }))
        .unwrap();

        assert!(filter.block_hash().is_none());
    }

    #[test]
    fn cast_log_subscription_filter_backfills_requested_range() {
        let filter = parse_subscribe_filter(&json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "eth_subscribe",
            "params": [
                "logs",
                {
                    "fromBlock": "earliest",
                    "toBlock": "latest",
                    "address": "0x0000000000000000000000000000000000000001",
                    "topics": [
                        "0x59950fb23669ee30425f6d79758e75fae698a6c88b2982f2980638d8bcd9397d"
                    ]
                }
            ]
        }))
        .unwrap();

        assert_eq!(
            initial_subscription_range(&filter, Some(12)).unwrap(),
            (Some((0, 12)), 13)
        );
    }

    #[test]
    fn unbounded_log_subscription_starts_after_latest_block() {
        let filter = parse_subscribe_filter(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_subscribe",
            "params": ["logs", { "topics": [null] }]
        }))
        .unwrap();

        assert_eq!(
            initial_subscription_range(&filter, Some(12)).unwrap(),
            (None, 13)
        );
    }

    #[test]
    fn rejects_unsupported_subscription_kind() {
        let err = parse_subscribe_filter(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_subscribe",
            "params": ["newHeads"]
        }))
        .unwrap_err();

        assert_eq!(
            err,
            invalid_params("unsupported subscription type: newHeads")
        );
    }

    #[test]
    fn parses_unsubscribe_id() {
        let subscription_id = parse_unsubscribe_id(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_unsubscribe",
            "params": ["0x1"]
        }))
        .unwrap();

        assert_eq!(subscription_id, "0x1");
    }

    #[test]
    fn websocket_origin_policy_follows_cors_config() {
        let no_browser_origins = WebSocketOriginPolicy::from_cors_header(None);
        assert!(websocket_origin_allowed(&no_browser_origins, None));
        assert!(!websocket_origin_allowed(
            &no_browser_origins,
            Some("https://example.com")
        ));

        let any_origin = WebSocketOriginPolicy::from_cors_header(Some(&CorsOrigin::Any));
        assert!(websocket_origin_allowed(&any_origin, None));
        assert!(websocket_origin_allowed(
            &any_origin,
            Some("https://example.com")
        ));

        let specified_origin = WebSocketOriginPolicy::from_cors_header(Some(
            &CorsOrigin::Specified("https://allowed.example".to_string()),
        ));
        assert!(websocket_origin_allowed(
            &specified_origin,
            Some("https://allowed.example")
        ));
        assert!(!websocket_origin_allowed(
            &specified_origin,
            Some("https://blocked.example")
        ));
    }

    #[test]
    fn block_event_advances_subscription_cursor() {
        assert_eq!(block_range_after_event(10, 9, None), None);
        assert_eq!(block_range_after_event(10, 10, None), Some((10, 10, 11)));
        assert_eq!(block_range_after_event(10, 12, None), Some((10, 12, 13)));
        assert_eq!(
            block_range_after_event(u64::MAX, u64::MAX, None),
            Some((u64::MAX, u64::MAX, u64::MAX))
        );
    }

    #[test]
    fn block_event_respects_finite_to_block() {
        assert_eq!(
            block_range_after_event(10, 12, Some(11)),
            Some((10, 11, 12))
        );
        assert_eq!(block_range_after_event(12, 13, Some(11)), None);
        assert!(subscription_is_complete(12, Some(11)));
    }
}
