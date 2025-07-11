use std::path::PathBuf;

use aes_gcm::{aead::Aead, KeyInit};
use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    routing::get,
    Router,
};
use axum_extra::extract::Host;
use futures::{stream::FuturesUnordered, StreamExt};
use reqwest::header::CONTENT_TYPE;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Cdn functionality. Urls of specific format lead to donwnlaoding of metadata from the registry,
/// and serving of chunks.
pub struct Cdn {
    cache: PathBuf,
    cancel_token: CancellationToken,
}

/// Cache for reconstructed blocks
#[derive(Clone)]
struct Cache {
    base: PathBuf,
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
    pub fn start(&self, listener: TcpListener) -> Result<(), Box<dyn std::error::Error>> {
        let state = Cache {
            base: self.cache.clone(),
        };

        if !self.cache.exists() {
            info!(dir = %self.cache.display(), "Creating cache dir");
            std::fs::create_dir(&self.cache)?;
        }

        if !self.cache.is_dir() {
            return Err("Cache dir is not a directory".into());
        }

        let router = Router::new().route("/", get(cdn)).with_state(state);

        let cancel_token = self.cancel_token.clone();

        tokio::spawn(async {
            axum::serve(listener, router)
                .with_graceful_shutdown(cancel_token.cancelled_owned())
                .await
                .map_err(|err| {
                    warn!(%err, "Cdn server error");
                })
        });

        Ok(())
    }
}

#[derive(Debug, serde::Deserialize)]
struct DecryptionKeyQuery {
    key: Option<String>,
}

#[tracing::instrument(level = tracing::Level::DEBUG, skip(cache))]
async fn cdn(
    Host(host): Host,
    Query(query): Query<DecryptionKeyQuery>,
    State(cache): State<Cache>,
) -> Result<(HeaderMap, Vec<u8>), StatusCode> {
    debug!("Received request at {host}");
    let mut parts = host.split('.');
    let prefix = parts
        .next()
        .expect("Splitting a String always yields at least 1 result; Qed.");
    if prefix.len() != 32 {
        return Err(StatusCode::BAD_REQUEST);
    }

    let mut hash = [0; 16];
    faster_hex::hex_decode(prefix.as_bytes(), &mut hash).map_err(|_| StatusCode::BAD_REQUEST)?;

    let registry_url = parts.collect::<Vec<_>>().join(".");

    let decryption_key = if let Some(query_key) = query.key {
        let mut key = [0; 16];
        faster_hex::hex_decode(query_key.as_bytes(), &mut key)
            .map_err(|_| StatusCode::BAD_REQUEST)?;
        Some(key)
    } else {
        None
    };

    let meta = load_meta(registry_url.clone(), hash, decryption_key).await?;
    debug!("Metadata loaded");

    let mut headers = HeaderMap::new();
    match meta {
        cdn_meta::Metadata::File(file) => {
            //
            if let Some(mime) = file.mime {
                debug!(%mime, "Setting mime type");
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
                content.extend_from_slice(cache.fetch_block(&block).await?.as_slice());
            }
            Ok((headers, content))
        }
        cdn_meta::Metadata::Directory(dir) => {
            let mut out = r#"
<!DOCTYPE html>

<html i18n-values="dir:textdirection;lang:language">
<head>
<meta charset="utf-8">
</head>
<body>
<ul>"#
                .to_string();
            headers.append(
                CONTENT_TYPE,
                "text/html"
                    .parse()
                    .expect("Can parse \"text/html\" to content-type"),
            );
            for (file_hash, encryption_key) in dir.files {
                let meta = load_meta(registry_url.clone(), file_hash, encryption_key).await?;
                let name = match meta {
                    cdn_meta::Metadata::File(file) => file.name,
                    cdn_meta::Metadata::Directory(dir) => dir.name,
                };
                out.push_str(&format!(
                    "<li><a href=\"http://{}.{registry_url}/?key={}\">{name}</a></li>\n",
                    faster_hex::hex_string(&file_hash),
                    &encryption_key
                        .map(|ek| faster_hex::hex_string(&ek))
                        .unwrap_or_else(String::new),
                ));
            }

            out.push_str("</ul></body></html>");
            Ok((headers, out.into()))
        }
    }
}

/// Load a metadata blob from a metadata repository.
async fn load_meta(
    registry_url: String,
    hash: cdn_meta::Hash,
    encryption_key: Option<cdn_meta::Hash>,
) -> Result<cdn_meta::Metadata, StatusCode> {
    let mut r_url = reqwest::Url::parse(&format!("http://{registry_url}")).map_err(|err| {
        error!(%err, "Could not parse registry URL");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;
    let hex_hash = faster_hex::hex_string(&hash);
    r_url.set_path(&format!("/api/v1/metadata/{hex_hash}"));
    r_url.set_scheme("http").map_err(|_| {
        error!("Could not set HTTP scheme");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    debug!(url = %r_url, "Fetching chunk");

    let metadata_reply = reqwest::get(r_url).await.map_err(|err| {
        error!(%err, "Could not load metadata from registry");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    // TODO: Should we just check if status code is success here?
    if metadata_reply.status() != StatusCode::OK {
        debug!(
            status = %metadata_reply.status(),
            "Registry replied with non-OK status code"
        );
        return Err(metadata_reply.status());
    }

    let encrypted_metadata = metadata_reply.bytes().await.map_err(|err| {
        error!(%err, "Could not load metadata response from registry");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    let metadata = if let Some(encryption_key) = encryption_key {
        if encrypted_metadata.len() < 12 {
            debug!("Attempting to decrypt metadata with inufficient size");
            return Err(StatusCode::UNPROCESSABLE_ENTITY);
        }

        let decryptor = aes_gcm::Aes128Gcm::new(&encryption_key.into());
        let plaintext = decryptor
            .decrypt(
                encrypted_metadata[encrypted_metadata.len() - 12..].into(),
                &encrypted_metadata[..encrypted_metadata.len() - 12],
            )
            .map_err(|_| {
                warn!("Decryption of block failed");
                // Either the decryption key is wrong or the blob is corrupt, we assume the
                // registry is not a fault so the decryption key is wrong, which is a user error.
                StatusCode::UNPROCESSABLE_ENTITY
            })?;

        plaintext
    } else {
        encrypted_metadata.into()
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

    Ok(meta)
}

impl Drop for Cdn {
    fn drop(&mut self) {
        self.cancel_token.cancel();
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

impl Cache {
    async fn fetch_block(&self, block: &cdn_meta::Block) -> Result<Vec<u8>, StatusCode> {
        let mut cached_file_path = self.base.clone();
        cached_file_path.push(faster_hex::hex_string(&block.encrypted_hash));
        // If we have the file in cache, just open it, load it, and return from there.
        if cached_file_path.exists() {
            return tokio::fs::read(&cached_file_path).await.map_err(|err| {
                error!(%err, "Could not load cached file");
                StatusCode::INTERNAL_SERVER_ERROR
            });
        }

        // File is not in cache, download and save

        // TODO: Rank based on expected latency
        // FIXME: Only download the required amount
        let mut shard_stream = block
            .shards
            .iter()
            .enumerate()
            .map(|(i, loc)| async move { (i, download_shard(loc, &block.encrypted_hash).await) })
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

        let decryptor = aes_gcm::Aes128Gcm::new(&block.content_hash.into());
        let c = decryptor
            .decrypt(&block.nonce.into(), encrypted_data.as_slice())
            .map_err(|err| {
                warn!(%err, "Decryption of content block failed");
                StatusCode::UNPROCESSABLE_ENTITY
            })?;

        // Save file to cache, this is not critical if it fails
        if let Err(err) = tokio::fs::write(&cached_file_path, &c).await {
            warn!(%err, "Could not write block to cache");
        };

        Ok(c)
    }
}
