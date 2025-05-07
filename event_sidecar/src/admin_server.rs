mod status;

use crate::{
    types::config::AdminApiServerConfig,
    utils::{Unexpected, root_filter},
};
use anyhow::{Error, anyhow};
use casper_event_types::SidecarEvent;
use casper_types::Timestamp;
use futures::{FutureExt, TryFutureExt};
use hyper::Server;
use metrics::metrics_summary;
use status::{status_filters, store_sse_connection_status};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener},
    process::ExitCode,
    time::Duration,
};
use tokio::sync::broadcast::{
    Receiver, Sender,
    error::RecvError::{Closed, Lagged},
};
use tower::{ServiceBuilder, buffer::Buffer, make::Shared};
use tracing::{debug, error, info};
use warp::{Filter, Rejection, Reply};

const BIND_ALL_INTERFACES: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
struct AdminServer {
    port: u16,
    max_concurrent_requests: u32,
    max_requests_per_second: u32,
    sidecar_event_sender: Sender<SidecarEvent>,
}

impl AdminServer {
    pub async fn start(&self) -> Result<(), Error> {
        let start_timestamp = Timestamp::now();
        let api = root_filter()
            .or(metrics_filter())
            .or(status_filters(start_timestamp));
        let address = SocketAddr::new(BIND_ALL_INTERFACES, self.port);
        let listener = TcpListener::bind(address)?;
        let mut futures = Vec::new();
        let warp_service = warp::service(api);
        let tower_service = ServiceBuilder::new()
            .concurrency_limit(self.max_concurrent_requests as usize)
            .rate_limit(
                u64::from(self.max_requests_per_second),
                Duration::from_secs(1),
            )
            .service(warp_service);
        info!(address = %address, "started Admin API server");
        let listening_loop = event_listening_loop(self.sidecar_event_sender.subscribe()).boxed();
        futures.push(listening_loop);
        let future = Server::from_tcp(listener)?
            .serve(Shared::new(Buffer::new(tower_service, 50)))
            .map_err(|e| anyhow!("admin api server stopped, reason: {e}"))
            .boxed();
        futures.push(future);
        futures::future::select_all(futures).await.0
    }
}

pub(crate) async fn event_listening_loop(
    mut sidecar_event_receiver: Receiver<SidecarEvent>,
) -> Result<(), Error> {
    loop {
        match sidecar_event_receiver.recv().await {
            Ok(msg) => match msg {
                SidecarEvent::SseNodeConnectionStatusChange { connection, status } => {
                    store_sse_connection_status(connection, status).await;
                }
                SidecarEvent::BlockAdded { .. } => {
                    //Do nothing, this event is of no interest here
                }
            },
            Err(e) => {
                match e {
                    Closed => {
                        return Err(anyhow!(
                            "admin api event_listening_loop failed, reason: {:?}",
                            e
                        ));
                    }
                    Lagged(_) => {
                        debug!("admin api event_listening_loop lagging");
                    }
                };
            }
        }
    }
}
pub async fn run_server(
    config: AdminApiServerConfig,
    sidecar_event_sender: Sender<SidecarEvent>,
) -> Result<ExitCode, Error> {
    if config.enable_server {
        AdminServer {
            port: config.port,
            max_concurrent_requests: config.max_concurrent_requests,
            max_requests_per_second: config.max_requests_per_second,
            sidecar_event_sender,
        }
        .start()
        .await
        .map(|()| ExitCode::SUCCESS)
    } else {
        info!("Admin API server is disabled. Skipping...");
        Ok(ExitCode::SUCCESS)
    }
}

/// Return metrics data at a given time.
/// Return: prometheus-formatted metrics data.
/// Example: curl http://127.0.0.1:18887/metrics
fn metrics_filter() -> impl Filter<Extract = (impl warp::Reply,), Error = warp::Rejection> + Clone {
    warp::path!("metrics")
        .and(warp::get())
        .and_then(metrics_handler)
}

async fn metrics_handler() -> Result<impl Reply, Rejection> {
    let res_custom = metrics_summary()
        .map_err(|err| warp::reject::custom(Unexpected(Error::msg(err.to_string()))))?;

    Ok(res_custom)
}

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use crate::{admin_server::run_server, types::config::AdminApiServerConfig};
    use portpicker::pick_unused_port;
    use reqwest::Response;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn given_config_should_start_admin_server() {
        let port = pick_unused_port().unwrap();
        let request_url = format!("http://localhost:{port}/metrics");
        let admin_config = AdminApiServerConfig {
            enable_server: true,
            port,
            max_concurrent_requests: 1,
            max_requests_per_second: 1,
        };
        let (tx, _) = tokio::sync::broadcast::channel(1);
        tokio::spawn(run_server(admin_config, tx));

        let response = fetch_metrics_data(&request_url).await;
        let text = response.text().await.unwrap();
        assert!(text.contains("process_cpu_seconds_total"));
    }

    async fn fetch_metrics_data(request_url: &String) -> Response {
        reqwest::Client::new()
            .get(request_url)
            .send()
            .await
            .expect("Error requesting the /metrics endpoint")
    }
}
