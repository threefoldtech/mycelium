use std::path::PathBuf;

use aes_gcm::{aead::Aead, KeyInit};
use axum::{
    extract::Query,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use axum_extra::extract::Host;
use futures::{stream::FuturesUnordered, StreamExt};
use reqwest::header::CONTENT_TYPE;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

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
) -> Result<(HeaderMap, Vec<u8>), StatusCode> {
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

    let encrypted_metadata = metadata_reply
        .bytes()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let metadata = if query.key.is_some() {
        todo!();
    } else {
        encrypted_metadata
    };

    // If the metadata is not decodable, this is not really our fault, but also not the necessarily
    // the users fault.
    let (meta, consumed) =
        cdn_meta::Metadata::from_binary(&metadata).map_err(|_| StatusCode::UNPROCESSABLE_ENTITY)?;
    if consumed != metadata.len() {
        warn!(
            metadata_length = metadata.len(),
            consumed, "Trailing binary metadata which wasn't decoded"
        );
    }

    let mut headers = HeaderMap::new();
    match meta {
        cdn_meta::Metadata::File(file) => {
            //
            if let Some(mime) = file.mime {
                headers.append(
                    CONTENT_TYPE,
                    mime.parse().map_err(|_| {
                        warn!("Not serving file with unprocessable mime type");
                        StatusCode::UNPROCESSABLE_ENTITY
                    })?,
                );
            }

            // File recombination
            let mut content = vec![];
            for block in file.blocks {
                // TODO: Rank based on expected latency
                // FIXME: Only download ther required amount
                let mut shard_stream =
                    block
                        .shards
                        .iter()
                        .enumerate()
                        .map(|(i, loc)| async move {
                            (i, download_shard(loc, &block.encrypted_hash).await)
                        })
                        .collect::<FuturesUnordered<_>>();
                let mut shards = vec![None; block.shards.len()];
                while let Some((idx, shard)) = shard_stream.next().await {
                    let shard = shard.map_err(|err| {
                        warn!(err, "Could not load shard");
                        StatusCode::INTERNAL_SERVER_ERROR
                    })?;
                    shards[idx] = Some(shard);
                }
                // recombine
                let encoder = reed_solomon_erasure::galois_8::ReedSolomon::new(
                    block.required_shards as usize,
                    block.shards.len() - block.required_shards as usize,
                )
                .map_err(|err| {
                    error!(%err, "Failed to construct erausre codec");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;

                encoder.reconstruct_data(&mut shards).map_err(|err| {
                    error!(%err, "Shard recombination failed");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;

                // SAFETY: Since decoding was succesfull, the first shards (data shards) must be
                // Option::Some
                let mut encrypted_data = shards
                    .into_iter()
                    .map(Option::unwrap)
                    .take(block.required_shards as usize)
                    .flatten()
                    .collect::<Vec<_>>();

                let padding_len = encrypted_data[encrypted_data.len() - 1] as usize;
                encrypted_data.resize(encrypted_data.len() - padding_len, 0);

                let decryptor = aes_gcm::Aes256Gcm::new(&block.content_hash.into());
                let c = decryptor
                    .decrypt(&block.nonce.into(), encrypted_data.as_slice())
                    .map_err(|err| {
                        warn!(%err, "Decryption of content block failed");
                        StatusCode::UNPROCESSABLE_ENTITY
                    })?;
                content.extend_from_slice(&c);
            }
            Ok((headers, content))
        }
        cdn_meta::Metadata::Directory(dir) => {
            // TODO: Technically this mime type is deprecated
            headers.append(
                CONTENT_TYPE,
                "text/directory"
                    .parse()
                    .expect("Can parse \"text/directory\" to content-type"),
            );
            //
            for file_hash in dir.files {
                todo!();
            }
            Ok((headers, vec![]))
        }
    }
}

impl Drop for Cdn {
    fn drop(&mut self) {
        self.cancel_token.cancel();
        todo!()
    }
}

/// Download a shard from a 0-db.
async fn download_shard(
    location: &cdn_meta::Location,
    key: &[u8],
) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let client = redis::Client::open(format!("redis://{}", location.host))?;
    let mut con = client.get_multiplexed_async_connection().await?;

    redis::cmd("SELECT")
        .arg(&location.namespace)
        .query_async::<()>(&mut con)
        .await?;

    Ok(redis::cmd("GET").arg(key).query_async(&mut con).await?)
}
