use std::{
    net::{IpAddr, SocketAddr},
    ops::Deref,
    time::Duration,
};

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageSendInfo {
    pub dst: MessageDestination,
    #[serde(with = "base64")]
    pub payload: Vec<u8>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageDestination {
    Ip(IpAddr),
    Pk(PublicKey),
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageReceiveInfo {
    pub id: MessageId,
    pub src_ip: IpAddr,
    pub src_pk: PublicKey,
    pub dst_ip: IpAddr,
    pub dst_pk: PublicKey,
    #[serde(with = "base64")]
    pub payload: Vec<u8>,
}

impl MessageDestination {
    /// Get the IP address of the destination.
    fn ip(self) -> IpAddr {
        match self {
            MessageDestination::Ip(ip) => ip,
            MessageDestination::Pk(pk) => IpAddr::V6(pk.address()),
        }
    }
}

impl Http {
    /// Spawns a new HTTP API server on the provided listening address.
    pub fn spawn(message_stack: MessageStack, listen_addr: &SocketAddr) -> Self {
        let server_state = HttpServerState { message_stack };
        let msg_routes = Router::new()
            .route("/messages", get(get_message).post(push_message))
            .route("/messages/status/:id", get(message_status))
            .route("/messages/reply/:id", post(reply_message))
            .with_state(server_state);
        let app = Router::new().nest("/api/v1", msg_routes);
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

#[derive(Deserialize)]
struct GetMessageQuery {
    peek: Option<bool>,
    timeout: Option<u64>,
    /// Optional filter for start of the message.
    filter: Option<String>,
}

impl GetMessageQuery {
    /// Did the query indicate we should peek the message instead of pop?
    fn peek(&self) -> bool {
        matches!(self.peek, Some(true))
    }

    /// Amount of seconds to hold and try and get values.
    fn timeout_secs(&self) -> u64 {
        self.timeout.unwrap_or(0)
    }
}

async fn get_message(
    State(state): State<HttpServerState>,
    Query(query): Query<GetMessageQuery>,
) -> Result<Json<MessageReceiveInfo>, StatusCode> {
    debug!(
        "Attempt to get message, peek {}, timeout {} seconds",
        query.peek(),
        query.timeout_secs()
    );
    // A timeout of 0 seconds essentially means get a message if there is one, and return
    // immediatly if there isn't. This is the result of the implementation of Timeout, which does a
    // poll of the internal future first, before polling the delay.
    tokio::time::timeout(
        Duration::from_secs(query.timeout_secs()),
        state
            .message_stack
            .message(!query.peek(), query.filter.map(String::into_bytes)),
    )
    .await
    .or(Err(StatusCode::NO_CONTENT))
    .map(|m| {
        Json(MessageReceiveInfo {
            id: m.id,
            src_ip: m.src_ip,
            src_pk: m.src_pk,
            dst_ip: m.dst_ip,
            dst_pk: m.dst_pk,
            payload: m.data,
        })
    })
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageIdReply {
    id: MessageId,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
#[serde(untagged)]
pub enum PushMessageResponse {
    Id(MessageIdReply),
    Reply(MessageReceiveInfo),
}

#[derive(Deserialize)]
struct PushMessageQuery {
    reply_timeout: Option<u64>,
}

impl PushMessageQuery {
    /// The user requested to wait for the reply or not.
    fn await_reply(&self) -> bool {
        self.reply_timeout.is_some()
    }

    /// Amount of seconds to wait for the reply.
    fn timeout(&self) -> u64 {
        self.reply_timeout.unwrap_or(0)
    }
}

async fn push_message(
    State(state): State<HttpServerState>,
    Query(query): Query<PushMessageQuery>,
    Json(message_info): Json<MessageSendInfo>,
) -> Result<(StatusCode, Json<PushMessageResponse>), StatusCode> {
    let dst = message_info.dst.ip();
    debug!(
        "Pushing new message of {} bytes to message stack for target {dst}",
        message_info.payload.len(),
    );

    let (id, sub) = state.message_stack.new_message(
        dst,
        message_info.payload,
        DEFAULT_MESSAGE_TRY_DURATION,
        query.await_reply(),
    );

    if !query.await_reply() {
        // If we don't wait for the reply just return here.
        return Ok((
            StatusCode::CREATED,
            Json(PushMessageResponse::Id(MessageIdReply { id })),
        ));
    }

    let mut sub = sub.unwrap();
    tokio::select! {
        sub_res = sub.changed() => {
            match sub_res {
                Ok(_) => {
                    if let Some(m) = sub.borrow().deref()  {
                        Ok((StatusCode::OK, Json(PushMessageResponse::Reply(MessageReceiveInfo {
                            id: m.id,
                            src_ip: m.src_ip,
                            src_pk: m.src_pk,
                            dst_ip: m.dst_ip,
                            dst_pk: m.dst_pk,
                            payload: m.data.clone(),
                        }))))
                    } else {
                        // This happens if a none value is send, which should not happen.
                        Err(StatusCode::INTERNAL_SERVER_ERROR)
                    }
                }
                Err(_)  => {
                    // This happens if the sender drops, which should not happen.
                    Err(StatusCode::INTERNAL_SERVER_ERROR)
                }
            }
        },
        _ = tokio::time::sleep(Duration::from_secs(query.timeout())) => {
            // Timeout expired while waiting for reply
            Ok((StatusCode::REQUEST_TIMEOUT, Json(PushMessageResponse::Id(MessageIdReply { id  }))))
        }
    }
}

async fn reply_message(
    State(state): State<HttpServerState>,
    Path(id): Path<MessageId>,
    Json(message_info): Json<MessageSendInfo>,
) -> StatusCode {
    let dst = message_info.dst.ip();
    debug!(
        "Pushing new reply to {} of {} bytes to message stack for target {dst}",
        id.as_hex(),
        message_info.payload.len(),
    );

    state
        .message_stack
        .reply_message(id, dst, message_info.payload, DEFAULT_MESSAGE_TRY_DURATION);

    StatusCode::NO_CONTENT
}

async fn message_status(
    State(state): State<HttpServerState>,
    Path(id): Path<MessageId>,
) -> Result<Json<MessageInfo>, StatusCode> {
    debug!("Fetching message status for message {}", id.as_hex());

    state
        .message_stack
        .message_info(id)
        .ok_or(StatusCode::NOT_FOUND)
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