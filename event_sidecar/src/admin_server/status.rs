use std::{
    collections::{BTreeMap, btree_map::Entry},
    convert::Infallible,
    option_env,
    sync::LazyLock,
    time::Duration,
};

use casper_event_types::SIDECAR_VERSION;
use casper_types::Timestamp;
use http::StatusCode;
use humantime::format_duration;
use serde::Serialize;
use tokio::sync::RwLock;
use warp::{Filter, reject::Rejection, reply::Reply};

#[derive(Clone, Debug, Serialize)]
struct SidecarStatus {
    build_version: String,
    uptime: String,
    sse_connection_statuses: BTreeMap<String, String>,
}

static SSE_CONNECTION_STATUS_REGISTRY: LazyLock<RwLock<BTreeMap<String, String>>> =
    LazyLock::new(|| RwLock::new(BTreeMap::new()));

pub async fn get_sse_connection_status() -> BTreeMap<String, String> {
    let lock = SSE_CONNECTION_STATUS_REGISTRY.read().await;
    lock.clone()
}
pub async fn store_sse_connection_status(node_connection: String, status_label: String) {
    let mut lock = SSE_CONNECTION_STATUS_REGISTRY.write().await;
    match lock.entry(node_connection) {
        Entry::Vacant(vacant_entry) => {
            vacant_entry.insert(status_label);
        }
        Entry::Occupied(mut occupied_entry) => {
            *occupied_entry.get_mut() = status_label;
        }
    }
}
pub fn status_filters(
    server_start_time: Timestamp,
) -> impl Filter<Extract = (impl Reply,), Error = Rejection> + Clone {
    warp::path!("status")
        .and(warp::get())
        .and(with_timestamp(server_start_time))
        .and_then(get_status)
}

pub(super) async fn get_status(server_start_time: Timestamp) -> Result<impl Reply, Rejection> {
    let mut build_version = SIDECAR_VERSION.to_string();
    let key: Option<&'static str> = option_env!("VERGEN_GIT_SHA");
    if let Some(git_sha) = key {
        build_version = format!("{}-{}", build_version, git_sha);
    };
    let uptime = format_duration(Duration::from(server_start_time.elapsed())).to_string();

    let data = SidecarStatus {
        build_version,
        uptime,
        sse_connection_statuses: get_sse_connection_status().await,
    };
    let json = warp::reply::json(&data);
    Ok(warp::reply::with_status(json, StatusCode::OK).into_response())
}

fn with_timestamp(
    timestamp: Timestamp,
) -> impl Filter<Extract = (Timestamp,), Error = Infallible> + Clone {
    warp::any().map(move || timestamp)
}
