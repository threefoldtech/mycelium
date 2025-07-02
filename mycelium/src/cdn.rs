use std::path::PathBuf;

use axum::{extract::Query, http::StatusCode, response::IntoResponse, routing::get, Router};
use axum_extra::extract::Host;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};

/// Cdn functionality. Urls of specific format lead to donwnlaoding of metadata from the registry,
/// and serving of chunks.
pub struct Cdn {
    cache: PathBuf,
    cancel_token: CancellationToken,
}

impl Cdn {
    pub fn new(cache: PathBuf) -> Self {
        let cancel_token = CancellationToken::new();
        Self {
            cache,
            cancel_token,
        }
    }

    /// Start the Cdn server. This future runs until the server is stopped.
    pub async fn start(&self, listener: TcpListener) -> Result<(), Box<dyn std::error::Error>> {
        let router = Router::new().route("/", get(cdn));
        Ok(axum::serve(listener, router)
            .with_graceful_shutdown(self.cancel_token.clone().cancelled_owned())
            .await?)
    }
}

#[derive(Debug, serde::Deserialize)]
struct DecryptionKeyQuery {
    key: Option<String>,
}

#[tracing::instrument(level = tracing::Level::DEBUG)]
async fn cdn(
    Host(host): Host,
    Query(query): Query<DecryptionKeyQuery>,
) -> Result<Vec<u8>, StatusCode> {
    debug!("Received request at {host}");
    let mut parts = host.split('.');
    let prefix = parts
        .next()
        .expect("Splitting a String always yields at least 1 result; Qed.");
    if prefix.len() != 64 {
        return Err(StatusCode::BAD_REQUEST);
    }

    let mut hash = [0; 32];
    faster_hex::hex_decode(prefix.as_bytes(), &mut hash).map_err(|_| StatusCode::BAD_REQUEST)?;

    let mut registry_url = parts.collect::<Vec<_>>().join(".");
    registry_url.push_str(&format!("/api/v1/metadata/{prefix}"));

    debug!(url = registry_url, "Fetching chunk");

    let metadata_reply = reqwest::get(registry_url).await.map_err(|err| {
        error!(%err, "Could not load metadata from registry");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // TODO: Should we just check if status code is success here?
    if metadata_reply.status() != StatusCode::OK {
        return Err(metadata_reply.status());
    }

    todo!();
}
