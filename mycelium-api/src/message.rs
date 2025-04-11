use std::{net::IpAddr, ops::Deref, time::Duration};

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use tracing::debug;

use mycelium::{
    crypto::PublicKey,
    message::{MessageId, MessageInfo},
    metrics::Metrics,
    subnet::Subnet,
};
use std::path::PathBuf;

use super::ServerState;

/// Default amount of time to try and send a message if it is not explicitly specified.
const DEFAULT_MESSAGE_TRY_DURATION: Duration = Duration::from_secs(60 * 5);

/// Return a router which has message endpoints and their handlers mounted.
pub fn message_router_v1<M>(server_state: ServerState<M>) -> Router
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/messages", get(get_message).post(push_message))
        .route("/messages/status/{id}", get(message_status))
        .route("/messages/reply/{id}", post(reply_message))
        // Topic configuration endpoints
        .route(
            "/messages/topics/default",
            get(get_default_topic_action).put(set_default_topic_action),
        )
        .route("/messages/topics", get(get_topics).post(add_topic))
        .route("/messages/topics/{topic}", delete(remove_topic))
        .route(
            "/messages/topics/{topic}/sources",
            get(get_topic_sources).post(add_topic_source),
        )
        .route(
            "/messages/topics/{topic}/sources/{subnet}",
            delete(remove_topic_source),
        )
        .route(
            "/messages/topics/{topic}/forward",
            get(get_topic_forward_socket)
                .put(set_topic_forward_socket)
                .delete(remove_topic_forward_socket),
        )
        .with_state(server_state)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageSendInfo {
    pub dst: MessageDestination,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "base64::optional_binary")]
    pub topic: Option<Vec<u8>>,
    #[serde(with = "base64::binary")]
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageDestination {
    Ip(IpAddr),
    Pk(PublicKey),
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageReceiveInfo {
    pub id: MessageId,
    pub src_ip: IpAddr,
    pub src_pk: PublicKey,
    pub dst_ip: IpAddr,
    pub dst_pk: PublicKey,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "base64::optional_binary")]
    pub topic: Option<Vec<u8>>,
    #[serde(with = "base64::binary")]
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

#[derive(Deserialize)]
struct GetMessageQuery {
    peek: Option<bool>,
    timeout: Option<u64>,
    /// Optional filter for start of the message, base64 encoded.
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(with = "base64::optional_binary")]
    topic: Option<Vec<u8>>,
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

async fn get_message<M>(
    State(state): State<ServerState<M>>,
    Query(query): Query<GetMessageQuery>,
) -> Result<Json<MessageReceiveInfo>, StatusCode>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
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
            .node
            .lock()
            .await
            .get_message(!query.peek(), query.topic),
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
            topic: if m.topic.is_empty() {
                None
            } else {
                Some(m.topic)
            },
            payload: m.data,
        })
    })
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageIdReply {
    pub id: MessageId,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
#[serde(untagged)]
pub enum PushMessageResponse {
    Reply(MessageReceiveInfo),
    Id(MessageIdReply),
}

#[derive(Clone, Deserialize)]
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

async fn push_message<M>(
    State(state): State<ServerState<M>>,
    Query(query): Query<PushMessageQuery>,
    Json(message_info): Json<MessageSendInfo>,
) -> Result<(StatusCode, Json<PushMessageResponse>), StatusCode>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    let dst = message_info.dst.ip();
    debug!(
        message.dst=%dst,
        message.len=message_info.payload.len(),
        "Pushing new message to stack",
    );

    let (id, sub) = match state.node.lock().await.push_message(
        dst,
        message_info.payload,
        message_info.topic,
        DEFAULT_MESSAGE_TRY_DURATION,
        query.await_reply(),
    ) {
        Ok((id, sub)) => (id, sub),
        Err(_) => {
            return Err(StatusCode::BAD_REQUEST);
        }
    };

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
                            topic: if m.topic.is_empty() { None } else { Some(m.topic.clone()) },
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

async fn reply_message<M>(
    State(state): State<ServerState<M>>,
    Path(id): Path<MessageId>,
    Json(message_info): Json<MessageSendInfo>,
) -> StatusCode
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    let dst = message_info.dst.ip();
    debug!(
        message.id=id.as_hex(),
        message.dst=%dst,
        message.len=message_info.payload.len(),
        "Pushing new reply to message stack",
    );

    state.node.lock().await.reply_message(
        id,
        dst,
        message_info.payload,
        DEFAULT_MESSAGE_TRY_DURATION,
    );

    StatusCode::NO_CONTENT
}

async fn message_status<M>(
    State(state): State<ServerState<M>>,
    Path(id): Path<MessageId>,
) -> Result<Json<MessageInfo>, StatusCode>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    debug!(message.id=%id.as_hex(), "Fetching message status");

    state
        .node
        .lock()
        .await
        .message_status(id)
        .ok_or(StatusCode::NOT_FOUND)
        .map(Json)
}

/// Module to implement base64 decoding and encoding
/// Sourced from https://users.rust-lang.org/t/serialize-a-vec-u8-to-json-as-base64/57781, with some
/// addaptions to work with the new version of the base64 crate
mod base64 {
    use base64::engine::{GeneralPurpose, GeneralPurposeConfig};
    use base64::{alphabet, Engine};

    const B64ENGINE: GeneralPurpose = base64::engine::general_purpose::GeneralPurpose::new(
        &alphabet::STANDARD,
        GeneralPurposeConfig::new(),
    );

    pub fn encode(input: &[u8]) -> String {
        B64ENGINE.encode(input)
    }

    pub fn decode(input: &[u8]) -> Result<Vec<u8>, base64::DecodeError> {
        B64ENGINE.decode(input)
    }

    pub mod binary {
        use super::B64ENGINE;
        use base64::Engine;
        use serde::{Deserialize, Serialize};
        use serde::{Deserializer, Serializer};

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

    pub mod optional_binary {
        use super::B64ENGINE;
        use base64::Engine;
        use serde::{Deserialize, Serialize};
        use serde::{Deserializer, Serializer};

        pub fn serialize<S: Serializer>(v: &Option<Vec<u8>>, s: S) -> Result<S::Ok, S::Error> {
            if let Some(v) = v {
                let base64 = B64ENGINE.encode(v);
                String::serialize(&base64, s)
            } else {
                <Option<String>>::serialize(&None, s)
            }
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Vec<u8>>, D::Error> {
            if let Some(base64) = <Option<String>>::deserialize(d)? {
                B64ENGINE
                    .decode(base64.as_bytes())
                    .map_err(serde::de::Error::custom)
                    .map(Option::Some)
            } else {
                Ok(None)
            }
        }
    }
}

// Topic configuration API

/// Response for the default topic action
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DefaultTopicActionResponse {
    accept: bool,
}

/// Request to set the default topic action
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DefaultTopicActionRequest {
    accept: bool,
}

/// Request to add a source to a topic whitelist
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TopicSourceRequest {
    subnet: String,
}

/// Request to set a forward socket for a topic
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TopicForwardSocketRequest {
    socket_path: String,
}

/// Get the default topic action (accept or reject)
async fn get_default_topic_action<M>(
    State(state): State<ServerState<M>>,
) -> Json<DefaultTopicActionResponse>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    debug!("Getting default topic action");
    let accept = state.node.lock().await.unconfigure_topic_action();
    Json(DefaultTopicActionResponse { accept })
}

/// Set the default topic action (accept or reject)
async fn set_default_topic_action<M>(
    State(state): State<ServerState<M>>,
    Json(request): Json<DefaultTopicActionRequest>,
) -> StatusCode
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    debug!(accept=%request.accept, "Setting default topic action");
    state
        .node
        .lock()
        .await
        .accept_unconfigured_topic(request.accept);
    StatusCode::NO_CONTENT
}

/// Get all whitelisted topics
async fn get_topics<M>(State(state): State<ServerState<M>>) -> Json<Vec<String>>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    debug!("Getting all whitelisted topics");
    let node = state.node.lock().await;

    // Get the whitelist from the node
    let topics = node.topics();

    // Convert to TopicInfo structs
    let topics: Vec<String> = topics.iter().map(|topic| base64::encode(topic)).collect();

    Json(topics)
}

/// Add a topic to the whitelist
async fn add_topic<M>(
    State(state): State<ServerState<M>>,
    Json(topic_info): Json<Vec<u8>>,
) -> StatusCode
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    debug!("Adding topic to whitelist");
    state.node.lock().await.add_topic_whitelist(topic_info);
    StatusCode::CREATED
}

/// Remove a topic from the whitelist
async fn remove_topic<M>(
    State(state): State<ServerState<M>>,
    Path(topic): Path<String>,
) -> Result<StatusCode, StatusCode>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    debug!("Removing topic from whitelist");

    // Decode the base64 topic
    let topic_bytes = match base64::decode(topic.as_bytes()) {
        Ok(bytes) => bytes,
        Err(_) => return Err(StatusCode::BAD_REQUEST),
    };

    state.node.lock().await.remove_topic_whitelist(topic_bytes);
    Ok(StatusCode::NO_CONTENT)
}

/// Get all sources for a topic
async fn get_topic_sources<M>(
    State(state): State<ServerState<M>>,
    Path(topic): Path<String>,
) -> Result<Json<Vec<String>>, StatusCode>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    debug!("Getting sources for topic");

    // Decode the base64 topic
    let topic_bytes = match base64::decode(topic.as_bytes()) {
        Ok(bytes) => bytes,
        Err(_) => return Err(StatusCode::BAD_REQUEST),
    };

    let node = state.node.lock().await;

    // Get the whitelist from the node
    let sources = node.topic_allowed_sources(&topic_bytes);

    // Find the topic in the whitelist
    if let Some(sources) = sources {
        let sources = sources.into_iter().map(|s| s.to_string()).collect();
        Ok(Json(sources))
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

/// Add a source to a topic whitelist
async fn add_topic_source<M>(
    State(state): State<ServerState<M>>,
    Path(topic): Path<String>,
    Json(request): Json<TopicSourceRequest>,
) -> Result<StatusCode, StatusCode>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    debug!("Adding source to topic whitelist");

    // Decode the base64 topic
    let topic_bytes = match base64::decode(topic.as_bytes()) {
        Ok(bytes) => bytes,
        Err(_) => return Err(StatusCode::BAD_REQUEST),
    };

    // Parse the subnet
    let subnet = match request.subnet.parse::<Subnet>() {
        Ok(subnet) => subnet,
        Err(_) => return Err(StatusCode::BAD_REQUEST),
    };

    state
        .node
        .lock()
        .await
        .add_topic_whitelist_src(topic_bytes, subnet);
    Ok(StatusCode::CREATED)
}

/// Remove a source from a topic whitelist
async fn remove_topic_source<M>(
    State(state): State<ServerState<M>>,
    Path((topic, subnet_str)): Path<(String, String)>,
) -> Result<StatusCode, StatusCode>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    debug!("Removing source from topic whitelist");

    // Decode the base64 topic
    let topic_bytes = match base64::decode(topic.as_bytes()) {
        Ok(bytes) => bytes,
        Err(_) => return Err(StatusCode::BAD_REQUEST),
    };

    // Parse the subnet
    let subnet = match subnet_str.parse::<Subnet>() {
        Ok(subnet) => subnet,
        Err(_) => return Err(StatusCode::BAD_REQUEST),
    };

    state
        .node
        .lock()
        .await
        .remove_topic_whitelist_src(topic_bytes, subnet);
    Ok(StatusCode::NO_CONTENT)
}

/// Get the forward socket for a topic
async fn get_topic_forward_socket<M>(
    State(state): State<ServerState<M>>,
    Path(topic): Path<String>,
) -> Result<Json<Option<String>>, StatusCode>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    debug!("Getting forward socket for topic");

    // Decode the base64 topic
    let topic_bytes = match base64::decode(topic.as_bytes()) {
        Ok(bytes) => bytes,
        Err(_) => return Err(StatusCode::BAD_REQUEST),
    };

    let node = state.node.lock().await;
    let socket_path = node
        .get_topic_forward_socket(&topic_bytes)
        .map(|p| p.to_string_lossy().to_string());

    Ok(Json(socket_path))
}

/// Set the forward socket for a topic
async fn set_topic_forward_socket<M>(
    State(state): State<ServerState<M>>,
    Path(topic): Path<String>,
    Json(request): Json<TopicForwardSocketRequest>,
) -> Result<StatusCode, StatusCode>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    debug!("Setting forward socket for topic");

    // Decode the base64 topic
    let topic_bytes = match base64::decode(topic.as_bytes()) {
        Ok(bytes) => bytes,
        Err(_) => return Err(StatusCode::BAD_REQUEST),
    };

    let socket_path = PathBuf::from(request.socket_path);
    state
        .node
        .lock()
        .await
        .set_topic_forward_socket(topic_bytes, socket_path);
    Ok(StatusCode::NO_CONTENT)
}

/// Remove the forward socket for a topic
async fn remove_topic_forward_socket<M>(
    State(state): State<ServerState<M>>,
    Path(topic): Path<String>,
) -> Result<StatusCode, StatusCode>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    debug!("Removing forward socket for topic");

    // Decode the base64 topic
    let topic_bytes = match base64::decode(topic.as_bytes()) {
        Ok(bytes) => bytes,
        Err(_) => return Err(StatusCode::BAD_REQUEST),
    };

    state
        .node
        .lock()
        .await
        .delete_topic_forward_socket(topic_bytes);
    Ok(StatusCode::NO_CONTENT)
}
