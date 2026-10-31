use std::time::Duration;

use habilis_network::iroh::Endpoint;

pub(super) async fn wait_online(endpoint: &Endpoint) {
    let _ = tokio::time::timeout(Duration::from_secs(5), endpoint.online()).await;
}
