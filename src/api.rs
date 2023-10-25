use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::get,
    Json, Router,
};
use log::{debug, error};
use serde::{Deserialize, Serialize};

use crate::{
    crypto::PublicKey,
    message::{MessageId, MessageInfo, MessageStack},
};

/// Default amount of time to try and send a message if it is not explicitly specified.
const DEFAULT_MESSAGE_TRY_DURATION: Duration = Duration::from_secs(60 * 5);

/// Http API server handle. The server is spawned in a background task. If this handle is dropped,
/// the server is terminated.
pub struct Http {
    /// Channel to send cancellation to the http api server. We just keep a reference to it since
    /// dropping it will also cancel the receiver and thus the server.
    _cancel_tx: tokio::sync::oneshot::Sender<()>,
}

#[derive(Clone)]
struct HttpServerState {
    /// Access to messages.
    message_stack: MessageStack,
}

#[derive(Debug, Deserialize)]
struct MessageSendInfo {
    dst: IpAddr,
    #[serde(with = "base64")]
    payload: Vec<u8>,
}

#[derive(Serialize)]
struct MessageReceiveInfo {
    id: MessageId,
    src: PublicKey,
    dst: PublicKey,
    #[serde(with = "base64")]
    payload: Vec<u8>,
}

impl Http {
    /// Spawns a new HTTP API server on the provided listening address.
    pub fn spawn(message_stack: MessageStack, listen_addr: &SocketAddr) -> Self {
        let server_state = HttpServerState { message_stack };
        let app = Router::new()
            .route("/messages", get(pop_message).post(push_message))
            .route("/messages/peek", get(peek_message))
            .route("/messages/status/:id", get(message_status))
            .with_state(server_state);
        let (_cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
        let server = axum::Server::bind(listen_addr)
            .serve(app.into_make_service())
            .with_graceful_shutdown(async {
                cancel_rx.await.ok();
            });

        tokio::spawn(async {
            if let Err(e) = server.await {
                error!("Http API server error: {e}");
            }
        });
        Http { _cancel_tx }
    }
}

async fn peek_message(
    State(state): State<HttpServerState>,
) -> Result<Json<MessageReceiveInfo>, StatusCode> {
    debug!("Attempt to peek message");
    state
        .message_stack
        .peek_message()
        .ok_or(StatusCode::NO_CONTENT)
        .map(|m| {
            Json(MessageReceiveInfo {
                id: m.id,
                src: m.src_pk,
                dst: m.dst_pk,
                payload: m.data,
            })
        })
}

async fn pop_message(
    State(state): State<HttpServerState>,
) -> Result<Json<MessageReceiveInfo>, StatusCode> {
    debug!("Attempt to pop message");
    state
        .message_stack
        .pop_message()
        .ok_or(StatusCode::NO_CONTENT)
        .map(|m| {
            Json(MessageReceiveInfo {
                id: m.id,
                src: m.src_pk,
                dst: m.dst_pk,
                payload: m.data,
            })
        })
}

#[derive(Serialize)]
struct PushMessageResponse {
    id: MessageId,
}
async fn push_message(
    State(state): State<HttpServerState>,
    Json(message_info): Json<MessageSendInfo>,
) -> Result<Json<PushMessageResponse>, StatusCode> {
    debug!(
        "Pushing new message of {} bytes to message stack for target {}",
        message_info.payload.len(),
        message_info.dst
    );

    let id = state.message_stack.push_message(
        message_info.dst,
        message_info.payload,
        DEFAULT_MESSAGE_TRY_DURATION,
    );

    Ok(Json(PushMessageResponse { id }))
}

async fn message_status(
    State(state): State<HttpServerState>,
    Path(id): Path<MessageId>,
) -> Result<Json<MessageInfo>, StatusCode> {
    debug!("Fetching message status for message {}", id.as_hex());

    state
        .message_stack
        .message_info(id)
        .ok_or(StatusCode::NO_CONTENT)
        .map(Json)
}

/// Module to implement base64 decoding and encoding
// Sourced from https://users.rust-lang.org/t/serialize-a-vec-u8-to-json-as-base64/57781, with some
// addaptions to work with the new version of the base64 crate
mod base64 {
    use base64::alphabet;
    use base64::engine::{GeneralPurpose, GeneralPurposeConfig};
    use base64::Engine;
    use serde::{Deserialize, Serialize};
    use serde::{Deserializer, Serializer};

    const B64ENGINE: GeneralPurpose = base64::engine::general_purpose::GeneralPurpose::new(
        &alphabet::STANDARD,
        GeneralPurposeConfig::new(),
    );

    #[allow(dead_code)]
    pub fn serialize<S: Serializer>(v: &Vec<u8>, s: S) -> Result<S::Ok, S::Error> {
        let base64 = B64ENGINE.encode(v);
        String::serialize(&base64, s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let base64 = String::deserialize(d)?;
        B64ENGINE
            .decode(base64.as_bytes())
            .map_err(serde::de::Error::custom)
    }
}
