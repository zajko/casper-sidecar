//! Types and functions used by the http server to manage the event-stream.

use super::endpoint::Endpoint;
#[cfg(feature = "additional-metrics")]
use crate::utils::start_metrics_thread;
use casper_event_types::{
    legacy_sse_data::LegacySseData,
    sse_data::{EventFilter, SseData},
    Filter as SseFilter,
};
use casper_types::{ProtocolVersion, Transaction};
use futures::{future, Stream, StreamExt};
use http::StatusCode;
use hyper::Body;
use serde::Serialize;
#[cfg(test)]
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, RwLock},
};
#[cfg(feature = "additional-metrics")]
use tokio::sync::mpsc::Sender;
use tokio::sync::{
    broadcast::{self, error::RecvError},
    mpsc::{self, UnboundedSender},
};
use tokio_stream::wrappers::{
    errors::BroadcastStreamRecvError, BroadcastStream, UnboundedReceiverStream,
};
use tracing::{debug, error, info, warn};
use warp::{
    filters::BoxedFilter,
    path,
    reject::Rejection,
    reply::Response,
    sse::{self, Event as WarpServerSentEvent},
    Filter, Reply,
};

/// The URL root path.
pub const SSE_API_ROOT_PATH: &str = "events";

/// The URL path part to subscribe to "backwards compatible" 'main' event stream.
/// It will check for events from the nodes firehose and those which can be translated to 1.x format will be translated.
pub const SSE_API_MAIN_PATH: &str = "main";
/// The URL path part to subscribe to only `DeployAccepted` events.
pub const SSE_API_DEPLOYS_PATH: &str = "deploys";
/// The URL path part to subscribe to only `FinalitySignature` events.
pub const SSE_API_SIGNATURES_PATH: &str = "sigs";

/// The URL path part to subscribe to all events other than `TransactionAccepted`s and
/// `FinalitySignature`s.
/// The URL path part to subscribe to sidecar specific events.
pub const SSE_API_SIDECAR_PATH: &str = "sidecar";
/// The URL query string field name.
pub const QUERY_FIELD: &str = "start_from";

// This notice should go away once we remove the legacy filter endpoints.
pub const LEGACY_ENDPOINT_NOTICE: &str = "This endpoint is NOT a 1 to 1 representation of events coming off of the node. Some events are not transformable to legacy format. Please consult the documentation for details. This endpoint is deprecated and will be removed in a future release. Please migrate to the /events endpoint instead.";

/// The filter associated with `/events` path.
const EVENTS_FILTER: [EventFilter; 8] = [
    EventFilter::ApiVersion,
    EventFilter::BlockAdded,
    EventFilter::TransactionAccepted,
    EventFilter::TransactionProcessed,
    EventFilter::TransactionExpired,
    EventFilter::Fault,
    EventFilter::FinalitySignature,
    EventFilter::Step,
];
/// The filter associated with `/events/main` path.
const MAIN_FILTER: [EventFilter; 6] = [
    EventFilter::ApiVersion,
    EventFilter::BlockAdded,
    EventFilter::TransactionProcessed,
    EventFilter::TransactionExpired,
    EventFilter::Fault,
    EventFilter::Step,
];
/// The filter associated with `/events/deploys` path.
const DEPLOYS_FILTER: [EventFilter; 2] =
    [EventFilter::ApiVersion, EventFilter::TransactionAccepted];
/// The filter associated with `/events/sigs` path.
const SIGNATURES_FILTER: [EventFilter; 2] =
    [EventFilter::ApiVersion, EventFilter::FinalitySignature];
/// The filter associated with `/events/sidecar` path.
const SIDECAR_FILTER: [EventFilter; 1] = [EventFilter::SidecarVersion];
/// The "id" field of the events sent on the event stream to clients.
pub type Id = u32;
pub type IsLegacyFilter = bool;
type UrlProps = (
    &'static [EventFilter],
    &'static Endpoint,
    Option<u32>,
    IsLegacyFilter,
);

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
pub(super) struct TransactionAccepted {
    pub(super) transaction_accepted: Arc<Transaction>,
}

/// The components of a single SSE.
#[derive(Clone, Debug)]
pub(super) struct ServerSentEvent {
    /// The ID should only be `None` where the `data` is `SseData::ApiVersion`.
    pub(super) id: Option<Id>,
    /// Payload of the event. This generally shouldn't be an Option, but untill we have legacy filter endpoints we need to be prepared to have a event that is a comment and has no data. When legacy filter endpoints go away this should be of type SseData
    pub(super) data: Option<SseData>,
    /// Comment of the event
    pub(super) comment: Option<&'static str>,
    /// Information which endpoint we got the event from
    pub(super) inbound_filter: Option<SseFilter>,
}

impl ServerSentEvent {
    /// The first event sent to every subscribing client.
    pub(super) fn initial_event(client_api_version: ProtocolVersion) -> Self {
        ServerSentEvent {
            id: None,
            comment: None,
            data: Some(SseData::ApiVersion(client_api_version)),
            inbound_filter: None,
        }
    }
    pub(super) fn sidecar_version_event(version: ProtocolVersion) -> Self {
        ServerSentEvent {
            id: None,
            comment: None,
            data: Some(SseData::SidecarVersion(version)),
            inbound_filter: None,
        }
    }
    pub(super) fn legacy_comment_event() -> Self {
        ServerSentEvent {
            id: None,
            comment: Some(LEGACY_ENDPOINT_NOTICE),
            data: None,
            inbound_filter: None,
        }
    }
}

/// The messages sent via the tokio broadcast channel to the handler of each client's SSE stream.
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub(super) enum BroadcastChannelMessage {
    /// The message should be sent to the client as an SSE with an optional ID.  The ID should only
    /// be `None` where the `data` is `SseData::ApiVersion`.
    ServerSentEvent(ServerSentEvent),
    /// The stream should terminate as the server is shutting down.
    ///
    /// Note: ideally, we'd just drop all the tokio broadcast channel senders to make the streams
    /// terminate naturally, but we can't drop the sender cloned into warp filter.
    Shutdown,
}

fn event_to_warp_event(
    event: &ServerSentEvent,
    data: &SseData,
    is_legacy_filter: bool,
    maybe_id: Option<String>,
) -> Option<Result<WarpServerSentEvent, RecvError>> {
    let warp_data = WarpServerSentEvent::default();
    let maybe_event = if is_legacy_filter {
        let legacy_data = LegacySseData::from(&data);
        legacy_data.map(|data| {
            warp_data.json_data(&data).unwrap_or_else(|error| {
                warn!(%error, ?event, "failed to jsonify sse event");
                WarpServerSentEvent::default()
            })
        })
    } else {
        Some(warp_data.json_data(&data).unwrap_or_else(|error| {
            warn!(%error, ?event, "failed to jsonify sse event");
            WarpServerSentEvent::default()
        }))
    };
    maybe_event.map(|mut event| {
        if let Some(id) = maybe_id {
            event = event.id(id);
        }
        Ok(event)
    })
}

/// Passed to the server whenever a new client subscribes.
pub(super) struct NewSubscriberInfo {
    /// The event ID from which the stream should start for this client.
    pub(super) start_from: Option<Id>,
    /// A channel to send the initial events to the client's handler.  This will always send the
    /// ApiVersion as the first event, and then any buffered events as indicated by `start_from`.
    pub(super) initial_events_sender: mpsc::UnboundedSender<ServerSentEvent>,
    pub(super) enable_legacy_filters: bool,
}

/// Filters the `event`, mapping it to a warp event, or `None` if it should be filtered out.
async fn filter_map_server_sent_event(
    event: &ServerSentEvent,
    stream_filter: &Endpoint,
    event_filter: &[EventFilter],
    is_legacy_filter: bool,
) -> Option<Result<WarpServerSentEvent, RecvError>> {
    if !event
        .data
        .as_ref()
        .map(|d| d.should_include(event_filter))
        .unwrap_or(true)
    {
        return None;
    }

    if !event
        .comment
        .as_ref()
        .map(|_| is_legacy_filter)
        .unwrap_or(true)
    {
        return None;
    }

    if let Some(data) = event.data.as_ref() {
        let id = match determine_id(event, data) {
            Some(id) => id,
            None => return None,
        };
        match data {
            &SseData::ApiVersion { .. } | &SseData::SidecarVersion { .. } => {
                event_to_warp_event(event, data, is_legacy_filter, None)
            }
            &SseData::BlockAdded { .. }
            | &SseData::TransactionProcessed { .. }
            | &SseData::TransactionExpired { .. }
            | &SseData::Fault { .. }
            | &SseData::Step { .. }
            | &SseData::TransactionAccepted(..)
            | &SseData::FinalitySignature(_) => {
                event_to_warp_event(event, data, is_legacy_filter, Some(id))
            }
            &SseData::Shutdown => {
                if should_send_shutdown(event, stream_filter) {
                    build_event_for_outbound(event, data, id)
                } else {
                    None
                }
            }
        }
    } else {
        if let Some(comment) = event.comment {
            build_comment_event(comment)
        } else {
            None
        }
    }
}

fn should_send_shutdown(event: &ServerSentEvent, stream_filter: &Endpoint) -> bool {
    match (&event.inbound_filter, stream_filter) {
        (None, Endpoint::Sidecar) => true,
        (Some(_), _) => true,
        (None, _) => false,
    }
}

fn determine_id(event: &ServerSentEvent, data: &SseData) -> Option<String> {
    match event.id {
        Some(id) => {
            if matches!(data, &SseData::ApiVersion { .. }) {
                error!("ApiVersion should have no event ID");
                return None;
            }
            Some(id.to_string())
        }
        None => {
            if !matches!(
                data,
                &SseData::ApiVersion { .. } | &SseData::SidecarVersion { .. }
            ) {
                error!("only ApiVersion and SidecarVersion may have no event ID");
                return None;
            }
            Some(String::new())
        }
    }
}

fn build_comment_event(comment: &str) -> Option<Result<WarpServerSentEvent, RecvError>> {
    Some(Ok(WarpServerSentEvent::default().comment(comment)))
}

fn build_event_for_outbound(
    event: &ServerSentEvent,
    data: &SseData,
    id: String,
) -> Option<Result<WarpServerSentEvent, RecvError>> {
    let json_value = serde_json::to_value(data).unwrap();
    Some(Ok(WarpServerSentEvent::default()
        .json_data(&json_value)
        .unwrap_or_else(|error| {
            warn!(%error, ?event, "failed to jsonify sse event");
            WarpServerSentEvent::default()
        })
        .id(id)))
}

pub(super) fn path_to_filter(
    path_param: &str,
    enable_legacy_filters: bool,
) -> Option<&'static Endpoint> {
    match path_param {
        SSE_API_ROOT_PATH => Some(&Endpoint::Events),
        SSE_API_MAIN_PATH if enable_legacy_filters => Some(&Endpoint::Main),
        SSE_API_DEPLOYS_PATH if enable_legacy_filters => Some(&Endpoint::Deploys),
        SSE_API_SIGNATURES_PATH if enable_legacy_filters => Some(&Endpoint::Sigs),
        SSE_API_SIDECAR_PATH => Some(&Endpoint::Sidecar),
        _ => None,
    }
}
/// Converts the final URL path element to a slice of `EventFilter`s.
pub(super) fn get_filter(
    path_param: &str,
    enable_legacy_filters: bool,
) -> Option<(&'static [EventFilter], bool)> {
    match path_param {
        SSE_API_ROOT_PATH => Some((&EVENTS_FILTER[..], false)),
        SSE_API_MAIN_PATH if enable_legacy_filters => Some((&MAIN_FILTER[..], true)),
        SSE_API_DEPLOYS_PATH if enable_legacy_filters => Some((&DEPLOYS_FILTER[..], true)),
        SSE_API_SIGNATURES_PATH if enable_legacy_filters => Some((&SIGNATURES_FILTER[..], true)),
        SSE_API_SIDECAR_PATH => Some((&SIDECAR_FILTER[..], false)),
        _ => None,
    }
}

/// Extracts the starting event ID from the provided query, or `None` if `query` is empty.
///
/// If `query` is not empty, returns a 422 response if `query` doesn't have exactly one entry,
/// "starts_from" mapped to a value representing an event ID.
fn parse_query(query: HashMap<String, String>) -> Result<Option<Id>, Response> {
    if query.is_empty() {
        return Ok(None);
    }

    if query.len() > 1 {
        return Err(create_422());
    }

    match query
        .get(QUERY_FIELD)
        .and_then(|id_str| id_str.parse::<Id>().ok())
    {
        Some(id) => Ok(Some(id)),
        None => Err(create_422()),
    }
}

/// Creates a 404 response with a useful error message in the body.
fn create_404(enable_legacy_filters: bool) -> Response {
    let text = if enable_legacy_filters {
        format!(
            "invalid path: expected '/{root}/{main}', '/{root}/{deploys}' or '/{root}/{sigs} or '/{root}/{sidecar}'\n",
            root = SSE_API_ROOT_PATH,
            main = SSE_API_MAIN_PATH,
            deploys = SSE_API_DEPLOYS_PATH,
            sigs = SSE_API_SIGNATURES_PATH,
            sidecar = SSE_API_SIDECAR_PATH,
        )
    } else {
        format!(
            "invalid path: expected '/{root}/{main}' or '/{root}/{sidecar}'\n",
            root = SSE_API_ROOT_PATH,
            main = SSE_API_MAIN_PATH,
            sidecar = SSE_API_SIDECAR_PATH,
        )
    };
    let mut response = Response::new(Body::from(text));
    *response.status_mut() = StatusCode::NOT_FOUND;
    response
}

/// Creates a 422 response with a useful error message in the body for use in case of a bad query
/// string.
fn create_422() -> Response {
    let mut response = Response::new(Body::from(format!(
        "invalid query: expected single field '{}=<EVENT ID>'\n",
        QUERY_FIELD
    )));
    *response.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
    response
}

/// Creates a 503 response (Service Unavailable) to be returned if the server has too many
/// subscribers.
fn create_503() -> Response {
    let mut response = Response::new(Body::from("server has reached limit of subscribers"));
    *response.status_mut() = StatusCode::SERVICE_UNAVAILABLE;
    response
}

pub(super) struct ChannelsAndFilter {
    pub(super) event_broadcaster: broadcast::Sender<BroadcastChannelMessage>,
    pub(super) new_subscriber_info_receiver: mpsc::UnboundedReceiver<NewSubscriberInfo>,
    pub(super) sse_filter: BoxedFilter<(Response,)>,
}

fn serve_sse_response_handler(
    maybe_path_param: Option<String>,
    query: HashMap<String, String>,
    cloned_broadcaster: tokio::sync::broadcast::Sender<BroadcastChannelMessage>,
    max_concurrent_subscribers: u32,
    new_subscriber_info_sender: UnboundedSender<NewSubscriberInfo>,
    enable_legacy_filters: bool,
    #[cfg(feature = "additional-metrics")] metrics_sender: Sender<()>,
) -> http::Response<Body> {
    if let Some(value) = validate(&cloned_broadcaster, max_concurrent_subscribers) {
        return value;
    }
    let (event_filter, stream_filter, start_from, is_legacy_filter) =
        match parse_url_props(maybe_path_param, query, enable_legacy_filters) {
            Ok(value) => value,
            Err(error_response) => return error_response,
        };

    // Create a channel for the client's handler to receive the stream of initial events.
    let (initial_events_sender, initial_events_receiver) = mpsc::unbounded_channel();

    // Supply the server with the sender part of the channel along with the client's
    // requested starting point.
    let new_subscriber_info = NewSubscriberInfo {
        start_from,
        initial_events_sender,
        enable_legacy_filters,
    };
    if new_subscriber_info_sender
        .send(new_subscriber_info)
        .is_err()
    {
        error!("failed to send new subscriber info");
    }

    // Create a channel for the client's handler to receive the stream of ongoing events.
    let ongoing_events_receiver = cloned_broadcaster.subscribe();

    sse::reply(sse::keep_alive().stream(stream_to_client(
        initial_events_receiver,
        ongoing_events_receiver,
        stream_filter,
        event_filter,
        is_legacy_filter,
        #[cfg(feature = "additional-metrics")]
        metrics_sender,
    )))
    .into_response()
}

fn parse_url_props(
    maybe_path_param: Option<String>,
    query: HashMap<String, String>,
    enable_legacy_filters: bool,
) -> Result<UrlProps, http::Response<Body>> {
    let path_param = maybe_path_param.unwrap_or_else(|| SSE_API_ROOT_PATH.to_string());
    let (event_filter, is_legacy_filter) =
        match get_filter(path_param.as_str(), enable_legacy_filters) {
            Some((filter, is_legacy_filter)) => (filter, is_legacy_filter),
            None => return Err(create_404(enable_legacy_filters)),
        };
    let stream_filter = match path_to_filter(path_param.as_str(), enable_legacy_filters) {
        Some(filter) => filter,
        None => return Err(create_404(enable_legacy_filters)),
    };
    let start_from = match parse_query(query) {
        Ok(maybe_id) => maybe_id,
        Err(error_response) => return Err(error_response),
    };
    Ok((event_filter, stream_filter, start_from, is_legacy_filter))
}

fn validate(
    cloned_broadcaster: &broadcast::Sender<BroadcastChannelMessage>,
    max_concurrent_subscribers: u32,
) -> Option<http::Response<Body>> {
    // If we already have the maximum number of subscribers, reject this new one.
    if cloned_broadcaster.receiver_count() >= max_concurrent_subscribers as usize {
        info!(
            %max_concurrent_subscribers,
            "event stream server has max subscribers: rejecting new one"
        );
        return Some(create_503());
    }
    None
}

impl ChannelsAndFilter {
    /// Creates the message-passing channels required to run the event-stream server and the warp
    /// filter for the event-stream server.
    pub(super) fn new(
        broadcast_channel_size: usize,
        max_concurrent_subscribers: u32,
        enable_legacy_filters: bool,
    ) -> Self {
        // Create a channel to broadcast new events to all subscribed clients' streams.
        let (event_broadcaster, _) = broadcast::channel(broadcast_channel_size);
        let cloned_broadcaster = event_broadcaster.clone();

        #[cfg(feature = "additional-metrics")]
        let tx = start_metrics_thread("pushing outbound_events".to_string());
        // Create a channel for `NewSubscriberInfo`s to pass the information required to handle a
        // new client subscription.
        let (new_subscriber_info_sender, new_subscriber_info_receiver) = mpsc::unbounded_channel();
        let opt = warp::path::param::<String>()
            .map(Some)
            .or_else(|_| async { Ok::<(Option<String>,), std::convert::Infallible>((None,)) });
        let sse_filter = warp::get()
            .and(warp::path!("events" / ..))
            .and(opt)
            .and(path::end())
            .and(warp::query())
            .map(
                move |maybe_path_param: Option<String>, query: HashMap<String, String>| {
                    let new_subscriber_info_sender_clone = new_subscriber_info_sender.clone();
                    serve_sse_response_handler(
                        maybe_path_param,
                        query,
                        cloned_broadcaster.clone(),
                        max_concurrent_subscribers,
                        new_subscriber_info_sender_clone,
                        enable_legacy_filters,
                        #[cfg(feature = "additional-metrics")]
                        tx.clone(),
                    )
                },
            )
            .or_else(
                move |_| async move { Ok::<_, Rejection>((create_404(enable_legacy_filters),)) },
            )
            .boxed();

        ChannelsAndFilter {
            event_broadcaster,
            new_subscriber_info_receiver,
            sse_filter,
        }
    }
}

/// This takes the two channel receivers and turns them into a stream of SSEs to the subscribed
/// client.
///
/// The initial events receiver (an mpsc receiver) is exhausted first, and contains an initial
/// `ApiVersion` message, followed by any historical events the client requested using the query
/// string.
///
/// The ongoing events channel (a broadcast receiver) is then consumed, and will remain in use until
/// either the client disconnects, or the server shuts down (indicated by sending a `Shutdown`
/// variant via the channel).  This channel will receive all SSEs created from the moment the client
/// subscribed to the server's event stream.
///
/// It also takes an `EventFilter` which causes events to which the client didn't subscribe to be
/// skipped.
fn stream_to_client(
    initial_events: mpsc::UnboundedReceiver<ServerSentEvent>,
    ongoing_events: broadcast::Receiver<BroadcastChannelMessage>,
    stream_filter: &'static Endpoint,
    event_filter: &'static [EventFilter],
    is_legacy_filter: bool,
    #[cfg(feature = "additional-metrics")] metrics_sender: Sender<()>,
) -> impl Stream<Item = Result<WarpServerSentEvent, RecvError>> + 'static {
    // Keep a record of the IDs of the events delivered via the `initial_events` receiver.
    let initial_stream_ids = Arc::new(RwLock::new(HashSet::new()));
    let cloned_initial_ids = Arc::clone(&initial_stream_ids);
    // Map the events arriving after the initial stream to the correct error type, filtering out any
    // that have already been sent in the initial stream.
    let ongoing_stream = BroadcastStream::new(ongoing_events)
        .filter_map(move |result| {
            let cloned_initial_ids = Arc::clone(&cloned_initial_ids);
            async move {
                match result {
                    Ok(BroadcastChannelMessage::ServerSentEvent(event)) => {
                        handle_sse_event(event, cloned_initial_ids)
                    }
                    Ok(BroadcastChannelMessage::Shutdown) => Some(Err(RecvError::Closed)),
                    Err(BroadcastStreamRecvError::Lagged(amount)) => handle_lagged(amount),
                }
            }
        })
        .take_while(|result| future::ready(!matches!(result, Err(RecvError::Closed))))
        .boxed();

    build_combined_events_stream(
        initial_events,
        initial_stream_ids,
        ongoing_stream,
        stream_filter,
        event_filter,
        is_legacy_filter,
    )
}

// Builds stream that serves the initial events followed by the ongoing ones, filtering as dictated by the `event_filter`.
fn build_combined_events_stream(
    initial_events: mpsc::UnboundedReceiver<ServerSentEvent>,
    initial_stream_ids: Arc<RwLock<HashSet<u32>>>,
    ongoing_stream: std::pin::Pin<
        Box<dyn Stream<Item = Result<ServerSentEvent, RecvError>> + Send>,
    >,
    stream_filter: &'static Endpoint,
    event_filter: &'static [EventFilter],
    is_legacy_filter: bool,
) -> impl Stream<Item = Result<WarpServerSentEvent, RecvError>> + 'static {
    UnboundedReceiverStream::new(initial_events)
        .map(move |event| {
            if let Some(id) = event.id {
                let _ = initial_stream_ids.write().unwrap().insert(id);
            }
            Ok(event)
        })
        .chain(ongoing_stream)
        .filter_map(move |result| {
            #[cfg(feature = "additional-metrics")]
            let metrics_sender = metrics_sender.clone();
            async move {
                #[cfg(feature = "additional-metrics")]
                let sender = metrics_sender;
                match result {
                    Ok(event) => {
                        let fitlered_data = filter_map_server_sent_event(
                            &event,
                            stream_filter,
                            event_filter,
                            is_legacy_filter,
                        )
                        .await;
                        #[cfg(feature = "additional-metrics")]
                        if let Some(_) = fitlered_data {
                            let _ = sender.clone().send(()).await;
                        }
                        #[allow(clippy::let_and_return)]
                        fitlered_data
                    }
                    Err(error) => Some(Err(error)),
                }
            }
        })
}

fn handle_lagged(amount: u64) -> Option<Result<ServerSentEvent, RecvError>> {
    info!(
        "client lagged by {} events - dropping event stream connection to client",
        amount
    );
    Some(Err(RecvError::Lagged(amount)))
}

fn handle_sse_event(
    event: ServerSentEvent,
    cloned_initial_ids: Arc<RwLock<HashSet<u32>>>,
) -> Option<Result<ServerSentEvent, RecvError>> {
    if let Some(id) = event.id {
        if cloned_initial_ids.read().unwrap().contains(&id) {
            debug!(event_id=%id, "skipped duplicate event");
            return None;
        }
    }
    Some(Ok(event))
}

#[cfg(test)]
mod tests {
    use super::*;
    use casper_types::{testing::TestRng, TransactionHash};
    use rand::Rng;
    use regex::Regex;
    use std::iter;
    #[cfg(feature = "additional-metrics")]
    use tokio::sync::mpsc::channel;
    // The number of events in the initial stream, excluding the very first `ApiVersion` one.
    const NUM_INITIAL_EVENTS: usize = 10;
    // The number of events in the ongoing stream, including any duplicated from the initial
    // stream.
    const NUM_ONGOING_EVENTS: usize = 20;

    async fn should_filter_out(event: &ServerSentEvent, filter: &'static [EventFilter]) {
        assert!(
            filter_map_server_sent_event(event, &Endpoint::Events, filter, false)
                .await
                .is_none(),
            "should filter out {:?} with {:?}",
            event,
            filter
        );
    }

    async fn should_not_filter_out(event: &ServerSentEvent, filter: &'static [EventFilter]) {
        assert!(
            filter_map_server_sent_event(event, &Endpoint::Events, filter, false)
                .await
                .is_some(),
            "should not filter out {:?} with {:?}",
            event,
            filter
        );
    }

    /// This test checks that events with correct IDs (i.e. all types have an ID except for
    /// `ApiVersion`) are filtered properly.
    #[allow(clippy::too_many_lines)]
    #[tokio::test]
    async fn should_filter_events_with_valid_ids() {
        let mut rng = TestRng::new();

        let api_version = ServerSentEvent {
            id: None,
            comment: None,
            data: Some(SseData::random_api_version(&mut rng)),
            inbound_filter: None,
        };
        let block_added = ServerSentEvent {
            id: Some(rng.gen()),
            comment: None,
            data: Some(SseData::random_block_added(&mut rng)),
            inbound_filter: None,
        };
        let (sse_data, transaction) = SseData::random_transaction_accepted(&mut rng);
        let transaction_accepted = ServerSentEvent {
            id: Some(rng.gen()),
            comment: None,
            data: Some(sse_data),
            inbound_filter: None,
        };
        let mut transactions = HashMap::new();
        let _ = transactions.insert(transaction.hash(), transaction);
        let transaction_processed = ServerSentEvent {
            id: Some(rng.gen()),
            comment: None,
            data: Some(SseData::random_transaction_processed(&mut rng)),
            inbound_filter: None,
        };
        let transaction_expired = ServerSentEvent {
            id: Some(rng.gen()),
            comment: None,
            data: Some(SseData::random_transaction_expired(&mut rng)),
            inbound_filter: None,
        };
        let fault = ServerSentEvent {
            id: Some(rng.gen()),
            comment: None,
            data: Some(SseData::random_fault(&mut rng)),
            inbound_filter: None,
        };
        let finality_signature = ServerSentEvent {
            id: Some(rng.gen()),
            comment: None,
            data: Some(SseData::random_finality_signature(&mut rng)),
            inbound_filter: None,
        };
        let step = ServerSentEvent {
            id: Some(rng.gen()),
            comment: None,
            data: Some(SseData::random_step(&mut rng)),
            inbound_filter: None,
        };
        let shutdown = ServerSentEvent {
            id: Some(rng.gen()),
            comment: None,
            data: Some(SseData::Shutdown),
            inbound_filter: Some(SseFilter::Events),
        };
        let sidecar_api_version = ServerSentEvent {
            id: Some(rng.gen()),
            comment: None,
            data: Some(SseData::random_sidecar_version(&mut rng)),
            inbound_filter: None,
        };

        should_not_filter_out(&api_version, &EVENTS_FILTER[..]).await;
        should_not_filter_out(&block_added, &EVENTS_FILTER[..]).await;
        should_not_filter_out(&transaction_accepted, &EVENTS_FILTER[..]).await;
        should_not_filter_out(&transaction_processed, &EVENTS_FILTER[..]).await;
        should_not_filter_out(&transaction_expired, &EVENTS_FILTER[..]).await;
        should_not_filter_out(&fault, &EVENTS_FILTER[..]).await;
        should_not_filter_out(&step, &EVENTS_FILTER[..]).await;
        should_not_filter_out(&shutdown, &EVENTS_FILTER).await;
        should_not_filter_out(&api_version, &EVENTS_FILTER[..]).await;
        should_not_filter_out(&finality_signature, &EVENTS_FILTER[..]).await;
        should_filter_out(&sidecar_api_version, &EVENTS_FILTER[..]).await;

        should_filter_out(&api_version, &SIDECAR_FILTER[..]).await;
        should_filter_out(&block_added, &SIDECAR_FILTER[..]).await;
        should_filter_out(&transaction_accepted, &SIDECAR_FILTER[..]).await;
        should_filter_out(&transaction_processed, &SIDECAR_FILTER[..]).await;
        should_filter_out(&transaction_expired, &SIDECAR_FILTER[..]).await;
        should_filter_out(&fault, &SIDECAR_FILTER[..]).await;
        should_filter_out(&step, &SIDECAR_FILTER[..]).await;
        should_filter_out(&api_version, &SIDECAR_FILTER[..]).await;
        should_filter_out(&finality_signature, &SIDECAR_FILTER[..]).await;
        should_not_filter_out(&shutdown, &SIDECAR_FILTER).await;
        should_not_filter_out(&sidecar_api_version, &SIDECAR_FILTER[..]).await;

        should_not_filter_out(&api_version, &MAIN_FILTER[..]).await;
        should_not_filter_out(&block_added, &MAIN_FILTER[..]).await;
        should_not_filter_out(&transaction_processed, &MAIN_FILTER[..]).await;
        should_not_filter_out(&transaction_expired, &MAIN_FILTER[..]).await;
        should_not_filter_out(&fault, &MAIN_FILTER[..]).await;
        should_not_filter_out(&step, &MAIN_FILTER[..]).await;
        should_not_filter_out(&shutdown, &MAIN_FILTER).await;

        should_filter_out(&transaction_accepted, &MAIN_FILTER[..]).await;
        should_filter_out(&finality_signature, &MAIN_FILTER[..]).await;

        should_not_filter_out(&api_version, &DEPLOYS_FILTER[..]).await;
        should_not_filter_out(&transaction_accepted, &DEPLOYS_FILTER[..]).await;
        should_not_filter_out(&shutdown, &DEPLOYS_FILTER[..]).await;

        should_filter_out(&block_added, &DEPLOYS_FILTER[..]).await;
        should_filter_out(&transaction_processed, &DEPLOYS_FILTER[..]).await;
        should_filter_out(&transaction_expired, &DEPLOYS_FILTER[..]).await;
        should_filter_out(&fault, &DEPLOYS_FILTER[..]).await;
        should_filter_out(&finality_signature, &DEPLOYS_FILTER[..]).await;
        should_filter_out(&step, &DEPLOYS_FILTER[..]).await;

        should_not_filter_out(&api_version, &SIGNATURES_FILTER[..]).await;
        should_not_filter_out(&finality_signature, &SIGNATURES_FILTER[..]).await;
        should_not_filter_out(&shutdown, &SIGNATURES_FILTER[..]).await;

        should_filter_out(&block_added, &SIGNATURES_FILTER[..]).await;
        should_filter_out(&transaction_accepted, &SIGNATURES_FILTER[..]).await;
        should_filter_out(&transaction_processed, &SIGNATURES_FILTER[..]).await;
        should_filter_out(&transaction_expired, &SIGNATURES_FILTER[..]).await;
        should_filter_out(&fault, &SIGNATURES_FILTER[..]).await;
        should_filter_out(&step, &SIGNATURES_FILTER[..]).await;
    }

    /// This test checks that events with incorrect IDs (i.e. no types have an ID except for
    /// `ApiVersion`) are filtered out.
    #[allow(clippy::too_many_lines)]
    #[tokio::test]
    async fn should_filter_events_with_invalid_ids() {
        let mut rng = TestRng::new();

        let malformed_api_version = ServerSentEvent {
            id: Some(rng.gen()),
            comment: None,
            data: Some(SseData::random_api_version(&mut rng)),
            inbound_filter: None,
        };
        let malformed_block_added = ServerSentEvent {
            id: None,
            comment: None,
            data: Some(SseData::random_block_added(&mut rng)),
            inbound_filter: None,
        };
        let (sse_data, transaction) = SseData::random_transaction_accepted(&mut rng);
        let malformed_transaction_accepted = ServerSentEvent {
            id: None,
            comment: None,
            data: Some(sse_data),
            inbound_filter: None,
        };
        let mut transactions = HashMap::new();
        let _ = transactions.insert(transaction.hash(), transaction);
        let malformed_transaction_processed = ServerSentEvent {
            id: None,
            comment: None,
            data: Some(SseData::random_transaction_processed(&mut rng)),
            inbound_filter: None,
        };
        let malformed_transaction_expired = ServerSentEvent {
            id: None,
            comment: None,
            data: Some(SseData::random_transaction_expired(&mut rng)),
            inbound_filter: None,
        };
        let malformed_fault = ServerSentEvent {
            id: None,
            comment: None,
            data: Some(SseData::random_fault(&mut rng)),
            inbound_filter: None,
        };
        let malformed_finality_signature = ServerSentEvent {
            id: None,
            comment: None,
            data: Some(SseData::random_finality_signature(&mut rng)),
            inbound_filter: None,
        };
        let malformed_step = ServerSentEvent {
            id: None,
            comment: None,
            data: Some(SseData::random_step(&mut rng)),
            inbound_filter: None,
        };
        let malformed_shutdown = ServerSentEvent {
            id: None,
            comment: None,
            data: Some(SseData::Shutdown),
            inbound_filter: None,
        };

        for filter in &[
            &EVENTS_FILTER[..],
            &SIDECAR_FILTER[..],
            &MAIN_FILTER[..],
            &DEPLOYS_FILTER[..],
            &SIGNATURES_FILTER[..],
        ] {
            should_filter_out(&malformed_api_version, filter).await;
            should_filter_out(&malformed_block_added, filter).await;
            should_filter_out(&malformed_transaction_accepted, filter).await;
            should_filter_out(&malformed_transaction_processed, filter).await;
            should_filter_out(&malformed_transaction_expired, filter).await;
            should_filter_out(&malformed_fault, filter).await;
            should_filter_out(&malformed_finality_signature, filter).await;
            should_filter_out(&malformed_step, filter).await;
            should_filter_out(&malformed_shutdown, filter).await;
        }
    }

    #[allow(clippy::too_many_lines)]
    async fn should_filter_duplicate_events(path_filter: &str, is_legacy_endpoint: bool) {
        let mut rng = TestRng::new();

        let mut transactions = HashMap::new();

        let initial_events: Vec<ServerSentEvent> =
            iter::once(ServerSentEvent::initial_event(ProtocolVersion::V1_0_0))
                .chain(make_random_events(
                    &mut rng,
                    0,
                    NUM_INITIAL_EVENTS,
                    path_filter,
                    &mut transactions,
                ))
                .collect();

        // Run three cases; where only a single event is duplicated, where five are duplicated, and
        // where the whole initial stream (except the `ApiVersion`) is duplicated.
        for duplicate_count in &[1, 5, NUM_INITIAL_EVENTS] {
            // Create the events with the requisite duplicates at the start of the collection.
            let ongoing_events = make_ongoing_events(
                &mut rng,
                *duplicate_count,
                &initial_events,
                path_filter,
                &mut transactions,
            );

            let (initial_events_sender, initial_events_receiver) = mpsc::unbounded_channel();
            let (ongoing_events_sender, ongoing_events_receiver) =
                broadcast::channel(NUM_INITIAL_EVENTS + NUM_ONGOING_EVENTS + 1);

            // Send all the events.
            for event in initial_events.iter().cloned() {
                initial_events_sender.send(event).unwrap();
            }
            for event in ongoing_events.iter().cloned() {
                let _ = ongoing_events_sender
                    .send(BroadcastChannelMessage::ServerSentEvent(event))
                    .unwrap();
            }
            // Drop the channel senders so that the chained receiver streams can both complete.
            drop(initial_events_sender);
            drop(ongoing_events_sender);

            let stream_filter = path_to_filter(path_filter, true).unwrap();
            #[cfg(feature = "additional-metrics")]
            let (tx, rx) = channel(1000);
            let (filter, is_legacy_filter) = get_filter(path_filter, true).unwrap();
            // Collect the events emitted by `stream_to_client()` - should not contain duplicates.
            let received_events: Vec<Result<WarpServerSentEvent, RecvError>> = stream_to_client(
                initial_events_receiver,
                ongoing_events_receiver,
                stream_filter,
                filter,
                is_legacy_filter,
                #[cfg(feature = "additional-metrics")]
                tx,
            )
            .collect()
            .await;

            // Create the expected collection of emitted events.
            let deduplicated_events: Vec<ServerSentEvent> = initial_events
                .iter()
                .take(initial_events.len() - duplicate_count)
                .cloned()
                .chain(ongoing_events)
                .collect();

            assert_eq!(received_events.len(), deduplicated_events.len());

            // Iterate the received and expected collections, asserting that each matches.  As we
            // don't have access to the internals of the `WarpServerSentEvent`s, assert using their
            // `String` representations.
            for (received_event, deduplicated_event) in
                received_events.iter().zip(deduplicated_events.iter())
            {
                let received_event = received_event.as_ref().unwrap();

                let expected_data = deduplicated_event.data.clone().unwrap();
                let mut received_event_str = received_event.to_string().trim().to_string();

                let ends_with_id = Regex::new(r"\nid:\d*$").unwrap();
                let starts_with_data = Regex::new(r"^data:").unwrap();
                if let Some(id) = deduplicated_event.id {
                    assert!(received_event_str.ends_with(format!("\nid:{}", id).as_str()));
                } else {
                    assert!(!ends_with_id.is_match(received_event_str.as_str()));
                };
                received_event_str = ends_with_id
                    .replace_all(received_event_str.as_str(), "")
                    .into_owned();
                received_event_str = starts_with_data
                    .replace_all(received_event_str.as_str(), "")
                    .into_owned();
                if is_legacy_endpoint {
                    let maybe_legacy = LegacySseData::from(&expected_data);
                    assert!(maybe_legacy.is_some());
                    let input_legacy = maybe_legacy.unwrap();
                    let got_legacy =
                        serde_json::from_str::<LegacySseData>(received_event_str.as_str()).unwrap();
                    assert_eq!(got_legacy, input_legacy);
                } else {
                    let received_data =
                        serde_json::from_str::<Value>(received_event_str.as_str()).unwrap();
                    let expected_data = serde_json::to_value(&expected_data).unwrap();
                    assert_eq!(expected_data, received_data);
                }
            }
        }
    }

    #[tokio::test]
    async fn should_filter_duplicate_main_events() {
        should_filter_duplicate_events(SSE_API_MAIN_PATH, true).await
    }
    /// This test checks that deploy-accepted events from the initial stream which are duplicated in
    /// the ongoing stream are filtered out.
    #[tokio::test]
    async fn should_filter_duplicate_deploys_events() {
        should_filter_duplicate_events(SSE_API_DEPLOYS_PATH, true).await
    }

    /// This test checks that signature events from the initial stream which are duplicated in the
    /// ongoing stream are filtered out.
    #[tokio::test]
    async fn should_filter_duplicate_signature_events() {
        should_filter_duplicate_events(SSE_API_SIGNATURES_PATH, true).await
    }

    /// This test checks that main events from the initial stream which are duplicated in the
    /// ongoing stream are filtered out.
    #[tokio::test]
    async fn should_filter_duplicate_firehose_events() {
        should_filter_duplicate_events(SSE_API_ROOT_PATH, false).await
    }

    // Returns `count` random SSE events.  The events will have sequential IDs starting from `start_id`, and if the path filter
    // indicates the events should be transaction-accepted ones, the corresponding random transactions
    // will be inserted into `transactions`.
    fn make_random_events(
        rng: &mut TestRng,
        start_id: Id,
        count: usize,
        path_filter: &str,
        transactions: &mut HashMap<TransactionHash, Transaction>,
    ) -> Vec<ServerSentEvent> {
        (start_id..(start_id + count as u32))
            .map(|id| {
                let data = match path_filter {
                    SSE_API_MAIN_PATH => make_legacy_compliant_random_block(rng),
                    SSE_API_DEPLOYS_PATH => {
                        let (event, transaction) = make_legacy_compliant_random_transaction(rng);
                        assert!(transactions
                            .insert(transaction.hash(), transaction)
                            .is_none());
                        event
                    }
                    SSE_API_SIGNATURES_PATH => SseData::random_finality_signature(rng),
                    SSE_API_ROOT_PATH => {
                        let discriminator = id % 3;
                        match discriminator {
                            0 => SseData::random_block_added(rng),
                            1 => {
                                let (event, transaction) =
                                    SseData::random_transaction_accepted(rng);
                                assert!(transactions
                                    .insert(transaction.hash(), transaction)
                                    .is_none());
                                event
                            }
                            2 => SseData::random_finality_signature(rng),
                            _ => unreachable!(),
                        }
                    }
                    _ => unreachable!(),
                };
                ServerSentEvent {
                    id: Some(id),
                    comment: None,
                    data: Some(data),
                    inbound_filter: None,
                }
            })
            .collect()
    }

    fn make_legacy_compliant_random_transaction(rng: &mut TestRng) -> (SseData, Transaction) {
        loop {
            let (event, transaction) = SseData::random_transaction_accepted(rng);
            let legacy = LegacySseData::from(&event);
            if legacy.is_some() {
                return (event, transaction);
            }
        }
    }

    fn make_legacy_compliant_random_block(rng: &mut TestRng) -> SseData {
        loop {
            let block = SseData::random_block_added(rng);
            let legacy = LegacySseData::from(&block);
            if legacy.is_some() {
                return block;
            }
        }
    }

    // Returns `NUM_ONGOING_EVENTS` random SSE events for the ongoing stream containing
    // duplicates taken from the end of the initial stream.  Allows for the full initial stream
    // to be duplicated except for its first event (the `ApiVersion` one) which has no ID.
    fn make_ongoing_events(
        rng: &mut TestRng,
        duplicate_count: usize,
        initial_events: &[ServerSentEvent],
        path_filter: &str,
        transactions: &mut HashMap<TransactionHash, Transaction>,
    ) -> Vec<ServerSentEvent> {
        assert!(duplicate_count < initial_events.len());
        let initial_skip_count = initial_events.len() - duplicate_count;
        let unique_start_id = initial_events.len() as Id - 1;
        let unique_count = NUM_ONGOING_EVENTS - duplicate_count;
        initial_events
            .iter()
            .skip(initial_skip_count)
            .cloned()
            .chain(make_random_events(
                rng,
                unique_start_id,
                unique_count,
                path_filter,
                transactions,
            ))
            .collect()
    }
}
