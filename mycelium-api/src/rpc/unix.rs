//! HTTP/1.1 JSON-RPC 2.0 server over a Unix domain socket (Hero rpc.sock).
//!
//! Implements the Hero socket specification for `rpc.sock`:
//!   POST /rpc                          — JSON-RPC 2.0 dispatch (single + batch)
//!   GET  /openrpc.json                 — OpenRPC 1.x document
//!   GET  /health                       — Health check
//!   GET  /.well-known/heroservice.json — Discovery manifest
//!
//! All served over HTTP/1.1 on a Unix domain socket at
//! `$HERO_SOCKET_DIR/mycelium/rpc.sock`.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
#[cfg(feature = "message")]
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
#[cfg(feature = "message")]
use mycelium::subnet::Subnet;
#[cfg(feature = "message")]
use std::ops::Deref;

use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tokio::time::Instant;
use tracing::{error, info};

use mycelium::endpoint::Endpoint;
use mycelium::metrics::Metrics;
use mycelium::peer_manager::{PeerExists, PeerNotFound};

use crate::{Info, Metric, PacketStatEntry, PacketStatistics, QueriedSubnet, Route, ServerState};

/// Handle that keeps the Unix socket server alive.
/// Aborts the listener task when dropped.
pub struct JsonRpcUnix {
    _handle: tokio::task::JoinHandle<()>,
}

impl Drop for JsonRpcUnix {
    fn drop(&mut self) {
        self._handle.abort();
    }
}

/// Spawn an HTTP/1.1 JSON-RPC server at `socket_path` (Hero rpc.sock convention).
///
/// Removes any stale socket file, creates parent directories, binds the
/// listener, and serves HTTP in a background task.
pub async fn spawn<M>(
    node: Arc<Mutex<mycelium::Node<M>>>,
    socket_path: PathBuf,
    managed: std::sync::Arc<std::sync::Mutex<crate::rpc::network::managed::ManagedState>>,
) -> JsonRpcUnix
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    let _ = std::fs::remove_file(&socket_path);
    if let Some(parent) = socket_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let listener = match UnixListener::bind(&socket_path) {
        Ok(l) => {
            info!(path = %socket_path.display(), "Hero RPC Unix socket listening");
            l
        }
        Err(e) => {
            error!(path = %socket_path.display(), err = %e, "Failed to bind Unix socket");
            return JsonRpcUnix {
                _handle: tokio::spawn(async {}),
            };
        }
    };

    let state: Arc<ServerState<M>> = Arc::new(ServerState { node, managed });

    let app = Router::new()
        .route("/rpc", post(rpc_handler::<M>))
        .route("/openrpc.json", get(openrpc_handler))
        .route("/health", get(health_handler))
        .route(
            "/.well-known/heroservice.json",
            get(discovery_handler),
        )
        .with_state(state);

    let handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app.into_make_service()).await {
            error!("Unix socket server error: {}", e);
        }
    });

    JsonRpcUnix { _handle: handle }
}

// ---------------------------------------------------------------------------
// Static handlers
// ---------------------------------------------------------------------------

async fn health_handler() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "mycelium",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

async fn discovery_handler() -> Json<Value> {
    Json(json!({
        "protocol": "openrpc",
        "name": "mycelium",
        "title": "Mycelium Network Daemon",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "Encrypted IPv6 overlay network daemon",
        "socket": "rpc"
    }))
}

async fn openrpc_handler() -> impl IntoResponse {
    match serde_json::from_str::<Value>(super::spec::OPENRPC_SPEC) {
        Ok(spec) => (StatusCode::OK, Json(spec)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("spec parse error: {e}"),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// JSON-RPC dispatch handler
// ---------------------------------------------------------------------------

/// Handle `POST /rpc` — JSON-RPC 2.0 single and batch requests.
///
/// - Single object request: dispatch and return 200 with response object.
/// - Single notification (no `id`): dispatch and return 204 No Content.
/// - Batch array: dispatch all, return 200 with array of responses (notifications omitted).
/// - Batch of all notifications: return 204 No Content.
async fn rpc_handler<M>(
    State(state): State<Arc<ServerState<M>>>,
    Json(body): Json<Value>,
) -> impl IntoResponse
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    match body {
        Value::Array(requests) => {
            if requests.is_empty() {
                return (
                    StatusCode::OK,
                    Json(json!({"jsonrpc":"2.0","error":{"code":-32600,"message":"Invalid Request: empty batch"},"id":null})),
                )
                    .into_response();
            }

            let mut responses: Vec<Value> = Vec::new();
            for req_val in requests {
                match serde_json::from_value::<RpcReq>(req_val) {
                    Ok(req) => {
                        let is_notification = req.id.is_none();
                        let resp = dispatch(req, &state).await;
                        if !is_notification {
                            responses.push(
                                serde_json::to_value(resp).unwrap_or(Value::Null),
                            );
                        }
                        // notifications: fire and omit from response
                    }
                    Err(_) => {
                        responses.push(
                            serde_json::to_value(RpcResp::parse_error())
                                .unwrap_or(Value::Null),
                        );
                    }
                }
            }

            if responses.is_empty() {
                // All notifications — no response body
                StatusCode::NO_CONTENT.into_response()
            } else {
                (StatusCode::OK, Json(Value::Array(responses))).into_response()
            }
        }

        Value::Object(_) => {
            match serde_json::from_value::<RpcReq>(body) {
                Ok(req) => {
                    let is_notification = req.id.is_none();
                    let resp = dispatch(req, &state).await;
                    if is_notification {
                        StatusCode::NO_CONTENT.into_response()
                    } else {
                        (
                            StatusCode::OK,
                            Json(serde_json::to_value(resp).unwrap_or(Value::Null)),
                        )
                            .into_response()
                    }
                }
                Err(_) => (
                    StatusCode::OK,
                    Json(
                        serde_json::to_value(RpcResp::parse_error()).unwrap_or(Value::Null),
                    ),
                )
                    .into_response(),
            }
        }

        _ => (
            StatusCode::OK,
            Json(serde_json::to_value(RpcResp::parse_error()).unwrap_or(Value::Null)),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// JSON-RPC message types
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize, Debug)]
struct RpcReq {
    #[allow(dead_code)]
    jsonrpc: String,
    method: String,
    #[serde(default)]
    params: Option<Value>,
    /// Absent → notification (None); present (even null) → request (Some)
    #[serde(default)]
    id: Option<Value>,
}

#[derive(serde::Serialize)]
struct RpcResp {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
    id: Value,
}

impl RpcResp {
    fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            result: Some(result),
            error: None,
            id,
        }
    }
    fn err(id: Value, code: i32, message: &str) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            result: None,
            error: Some(json!({"code": code, "message": message})),
            id,
        }
    }
    fn method_not_found(id: Value) -> Self {
        Self::err(id, -32601, "Method not found")
    }
    fn invalid_params(id: Value, msg: &str) -> Self {
        Self::err(id, -32602, msg)
    }
    fn parse_error() -> Self {
        Self::err(Value::Null, -32700, "Parse error")
    }
}

// ---------------------------------------------------------------------------
// Param extraction helpers
// ---------------------------------------------------------------------------

/// Extract a string from positional array params[pos] or named object params[key].
fn str_param(params: &Option<Value>, pos: usize, key: &str) -> Option<String> {
    match params.as_ref()? {
        Value::Array(a) => a.get(pos)?.as_str().map(str::to_string),
        Value::Object(o) => o.get(key)?.as_str().map(str::to_string),
        _ => None,
    }
}

/// Extract an optional SocketAddr from params[pos] or params[key].
fn opt_socket_addr_param(params: &Option<Value>, pos: usize, key: &str) -> Option<SocketAddr> {
    str_param(params, pos, key).and_then(|s| s.parse().ok())
}

/// Extract an optional boolean from params[pos] or params[key].
fn opt_bool_param(params: &Option<Value>, pos: usize, key: &str) -> Option<bool> {
    match params.as_ref()? {
        Value::Array(a) => a.get(pos)?.as_bool(),
        Value::Object(o) => o.get(key)?.as_bool(),
        _ => None,
    }
}

/// Extract an optional u32 from params[pos] or params[key].
fn opt_u32_param(params: &Option<Value>, pos: usize, key: &str) -> Option<u32> {
    match params.as_ref()? {
        Value::Array(a) => a.get(pos)?.as_u64().and_then(|v| u32::try_from(v).ok()),
        Value::Object(o) => o.get(key)?.as_u64().and_then(|v| u32::try_from(v).ok()),
        _ => None,
    }
}

/// Extract an optional u8 from params[pos] or params[key].
fn opt_u8_param(params: &Option<Value>, pos: usize, key: &str) -> Option<u8> {
    match params.as_ref()? {
        Value::Array(a) => a.get(pos)?.as_u64().and_then(|v| u8::try_from(v).ok()),
        Value::Object(o) => o.get(key)?.as_u64().and_then(|v| u8::try_from(v).ok()),
        _ => None,
    }
}

/// Extract an optional Vec<String> from params[key] (or params[pos] if array).
fn opt_str_vec_param(params: &Option<Value>, pos: usize, key: &str) -> Option<Vec<String>> {
    let v = match params.as_ref()? {
        Value::Array(a) => a.get(pos)?,
        Value::Object(o) => o.get(key)?,
        _ => return None,
    };
    let arr = v.as_array()?;
    Some(
        arr.iter()
            .filter_map(|x| x.as_str().map(str::to_string))
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

async fn dispatch<M>(req: RpcReq, state: &Arc<ServerState<M>>) -> RpcResp
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    let id = req.id.unwrap_or(Value::Null);

    match req.method.as_str() {
        // ── OpenRPC discovery ────────────────────────────────────────────────
        "rpc.discover" => match serde_json::from_str::<Value>(super::spec::OPENRPC_SPEC) {
            Ok(spec) => RpcResp::ok(id, spec),
            Err(e) => RpcResp::err(id, -32603, &format!("spec parse error: {e}")),
        },

        // ── Admin ────────────────────────────────────────────────────────────
        "getInfo" => {
            let node_info = state.node.lock().await.info();
            let info = Info {
                node_subnet: node_info.node_subnet.to_string(),
                node_pubkey: node_info.node_pubkey,
            };
            match serde_json::to_value(info) {
                Ok(v) => RpcResp::ok(id, v),
                Err(e) => RpcResp::err(id, -32603, &e.to_string()),
            }
        }

        "getPublicKeyFromIp" => {
            let ip_str = match str_param(&req.params, 0, "ip") {
                Some(s) => s,
                None => return RpcResp::invalid_params(id, "missing 'ip' parameter"),
            };
            let ip: IpAddr = match ip_str.parse() {
                Ok(a) => a,
                Err(_) => return RpcResp::invalid_params(id, "invalid IP address"),
            };
            match state.node.lock().await.get_pubkey_from_ip(ip) {
                Some(pk) => match serde_json::to_value(pk) {
                    Ok(v) => RpcResp::ok(id, v),
                    Err(e) => RpcResp::err(id, -32603, &e.to_string()),
                },
                None => RpcResp::err(id, -32008, "public key not found for IP"),
            }
        }

        // ── Peers ────────────────────────────────────────────────────────────
        "getPeers" => {
            let peers = state.node.lock().await.peer_info();
            RpcResp::ok(id, serde_json::to_value(peers).unwrap_or(json!([])))
        }

        "addPeer" => {
            let ep_str = match str_param(&req.params, 0, "endpoint") {
                Some(s) => s,
                None => return RpcResp::invalid_params(id, "missing 'endpoint' parameter"),
            };
            let ep = match Endpoint::from_str(&ep_str) {
                Ok(e) => e,
                Err(_) => return RpcResp::invalid_params(id, "invalid endpoint format"),
            };
            match state.node.lock().await.add_peer(ep) {
                Ok(()) => RpcResp::ok(id, json!(true)),
                Err(PeerExists) => RpcResp::err(id, -32010, "peer already exists"),
            }
        }

        "deletePeer" => {
            let ep_str = match str_param(&req.params, 0, "endpoint") {
                Some(s) => s,
                None => return RpcResp::invalid_params(id, "missing 'endpoint' parameter"),
            };
            let ep = match Endpoint::from_str(&ep_str) {
                Ok(e) => e,
                Err(_) => return RpcResp::invalid_params(id, "invalid endpoint format"),
            };
            match state.node.lock().await.remove_peer(ep) {
                Ok(()) => RpcResp::ok(id, json!(true)),
                Err(PeerNotFound) => RpcResp::err(id, -32011, "peer not found"),
            }
        }

        // ── Routes ───────────────────────────────────────────────────────────
        "getSelectedRoutes" => {
            let routes: Vec<Route> = state
                .node
                .lock()
                .await
                .selected_routes()
                .into_iter()
                .map(|sr| Route {
                    subnet: sr.source().subnet().to_string(),
                    next_hop: sr.neighbour().connection_identifier().clone(),
                    metric: if sr.metric().is_infinite() {
                        Metric::Infinite
                    } else {
                        Metric::Value(sr.metric().into())
                    },
                    seqno: sr.seqno().into(),
                })
                .collect();
            RpcResp::ok(id, serde_json::to_value(routes).unwrap_or(json!([])))
        }

        "getFallbackRoutes" => {
            let routes: Vec<Route> = state
                .node
                .lock()
                .await
                .fallback_routes()
                .into_iter()
                .map(|sr| Route {
                    subnet: sr.source().subnet().to_string(),
                    next_hop: sr.neighbour().connection_identifier().clone(),
                    metric: if sr.metric().is_infinite() {
                        Metric::Infinite
                    } else {
                        Metric::Value(sr.metric().into())
                    },
                    seqno: sr.seqno().into(),
                })
                .collect();
            RpcResp::ok(id, serde_json::to_value(routes).unwrap_or(json!([])))
        }

        "getQueriedSubnets" => {
            let now = Instant::now();
            let queries: Vec<QueriedSubnet> = state
                .node
                .lock()
                .await
                .queried_subnets()
                .into_iter()
                .map(|qs| QueriedSubnet {
                    subnet: qs.subnet().to_string(),
                    expiration: qs
                        .query_expires()
                        .duration_since(now)
                        .as_secs()
                        .to_string(),
                })
                .collect();
            RpcResp::ok(id, serde_json::to_value(queries).unwrap_or(json!([])))
        }

        // ── Proxy ────────────────────────────────────────────────────────────
        "getProxies" => {
            let proxies = state.node.lock().await.known_proxies();
            RpcResp::ok(id, serde_json::to_value(proxies).unwrap_or(json!([])))
        }

        "connectProxy" => {
            let remote = opt_socket_addr_param(&req.params, 0, "remote");
            match state.node.lock().await.connect_proxy(remote).await {
                Ok(addr) => RpcResp::ok(id, json!(addr.to_string())),
                Err(_) => RpcResp::err(id, -32032, "no valid proxy available"),
            }
        }

        "disconnectProxy" => {
            state.node.lock().await.disconnect_proxy();
            RpcResp::ok(id, json!(true))
        }

        "startProxyProbe" => {
            state.node.lock().await.start_proxy_scan();
            RpcResp::ok(id, json!(true))
        }

        "stopProxyProbe" => {
            state.node.lock().await.stop_proxy_scan();
            RpcResp::ok(id, json!(true))
        }

        // ── Statistics ───────────────────────────────────────────────────────
        "getPacketStatistics" => {
            let stats = state.node.lock().await.packet_statistics();
            let result = PacketStatistics {
                by_source: stats
                    .by_source
                    .into_iter()
                    .map(|ps| PacketStatEntry {
                        ip: ps.ip.to_string(),
                        packet_count: ps.packet_count,
                        byte_count: ps.byte_count,
                    })
                    .collect(),
                by_destination: stats
                    .by_destination
                    .into_iter()
                    .map(|ps| PacketStatEntry {
                        ip: ps.ip.to_string(),
                        packet_count: ps.packet_count,
                        byte_count: ps.byte_count,
                    })
                    .collect(),
            };
            match serde_json::to_value(result) {
                Ok(v) => RpcResp::ok(id, v),
                Err(e) => RpcResp::err(id, -32603, &e.to_string()),
            }
        }

        #[cfg(feature = "message")]
        "popMessage" => {
            // params: peek? (bool), timeout? (u64 seconds), topic? (base64 string)
            let peek = match &req.params {
                Some(Value::Object(o)) => o.get("peek").and_then(|v| v.as_bool()).unwrap_or(false),
                Some(Value::Array(a)) => a.get(0).and_then(|v| v.as_bool()).unwrap_or(false),
                _ => false,
            };
            let timeout_secs = match &req.params {
                Some(Value::Object(o)) => o.get("timeout").and_then(|v| v.as_u64()),
                Some(Value::Array(a)) => a.get(1).and_then(|v| v.as_u64()),
                _ => None,
            };
            let topic_b64 = str_param(&req.params, 2, "topic");
            let topic_bytes = match topic_b64 {
                Some(ref s) => match BASE64.decode(s.as_bytes()) {
                    Ok(b) => Some(b),
                    Err(_) => return RpcResp::invalid_params(id, "topic: invalid base64"),
                },
                None => None,
            };
            let duration = tokio::time::Duration::from_secs(timeout_secs.unwrap_or(0));
            let result = tokio::time::timeout(
                duration,
                state.node.lock().await.get_message(!peek, topic_bytes),
            ).await;
            match result {
                Ok(m) => {
                    use crate::message::MessageReceiveInfo;
                    let info = MessageReceiveInfo {
                        id: m.id,
                        src_ip: m.src_ip,
                        src_pk: m.src_pk,
                        dst_ip: m.dst_ip,
                        dst_pk: m.dst_pk,
                        topic: if m.topic.is_empty() { None } else { Some(m.topic) },
                        payload: m.data,
                    };
                    match serde_json::to_value(info) {
                        Ok(v) => RpcResp::ok(id, v),
                        Err(e) => RpcResp::err(id, -32603, &e.to_string()),
                    }
                }
                Err(_) => RpcResp::ok(id, Value::Null), // timeout = no message
            }
        }

        #[cfg(feature = "message")]
        "pushMessage" => {
            use crate::message::{MessageSendInfo, MessageDestination};
            let message: MessageSendInfo = match req.params.as_ref().and_then(|p| serde_json::from_value(p.clone()).ok()) {
                Some(m) => m,
                None => return RpcResp::invalid_params(id, "invalid message params"),
            };
            let reply_timeout = match &req.params {
                Some(Value::Object(o)) => o.get("reply_timeout").and_then(|v| v.as_u64()),
                _ => None,
            };
            let dst = match message.dst {
                MessageDestination::Ip(ip) => ip,
                MessageDestination::Pk(pk) => std::net::IpAddr::V6(pk.address()),
            };
            const DEFAULT_TRY: tokio::time::Duration = tokio::time::Duration::from_secs(300);
            let result = state.node.lock().await.push_message(
                dst, message.payload, message.topic, DEFAULT_TRY, reply_timeout.is_some(),
            );
            let (msg_id, sub) = match result {
                Ok(r) => r,
                Err(_) => return RpcResp::err(id, -32015, "failed to push message"),
            };
            if reply_timeout.is_none() {
                return RpcResp::ok(id, serde_json::json!({"id": msg_id}));
            }
            let mut sub = sub.unwrap();
            tokio::select! {
                sub_res = sub.changed() => {
                    match sub_res {
                        Ok(_) => {
                            if let Some(m) = sub.borrow().deref() {
                                use crate::message::MessageReceiveInfo;
                                let info = MessageReceiveInfo {
                                    id: m.id, src_ip: m.src_ip, src_pk: m.src_pk,
                                    dst_ip: m.dst_ip, dst_pk: m.dst_pk,
                                    topic: if m.topic.is_empty() { None } else { Some(m.topic.clone()) },
                                    payload: m.data.clone(),
                                };
                                match serde_json::to_value(info) {
                                    Ok(v) => RpcResp::ok(id, v),
                                    Err(e) => RpcResp::err(id, -32603, &e.to_string()),
                                }
                            } else {
                                RpcResp::err(id, -32016, "unexpected empty reply")
                            }
                        }
                        Err(_) => RpcResp::err(id, -32017, "reply channel closed"),
                    }
                }
                _ = tokio::time::sleep(tokio::time::Duration::from_secs(reply_timeout.unwrap_or(0))) => {
                    RpcResp::ok(id, serde_json::json!({"id": msg_id}))
                }
            }
        }

        #[cfg(feature = "message")]
        "pushMessageReply" => {
            let reply_id_str = match str_param(&req.params, 0, "id") {
                Some(s) => s,
                None => return RpcResp::invalid_params(id, "missing 'id'"),
            };
            let message_id = match mycelium::message::MessageId::from_hex(reply_id_str.as_bytes()) {
                Ok(mid) => mid,
                Err(_) => return RpcResp::invalid_params(id, "invalid message id"),
            };
            use crate::message::{MessageSendInfo, MessageDestination};
            let message: MessageSendInfo = match req.params.as_ref().and_then(|p|
                if let Value::Object(o) = p { o.get("message").and_then(|m| serde_json::from_value(m.clone()).ok()) }
                else { None }
            ) {
                Some(m) => m,
                None => return RpcResp::invalid_params(id, "missing 'message' param"),
            };
            let dst = match message.dst {
                MessageDestination::Ip(ip) => ip,
                MessageDestination::Pk(pk) => std::net::IpAddr::V6(pk.address()),
            };
            const DEFAULT_TRY: tokio::time::Duration = tokio::time::Duration::from_secs(300);
            state.node.lock().await.reply_message(message_id, dst, message.payload, DEFAULT_TRY);
            RpcResp::ok(id, json!(true))
        }

        #[cfg(feature = "message")]
        "getMessageInfo" => {
            let msg_id_str = match str_param(&req.params, 0, "id") {
                Some(s) => s,
                None => return RpcResp::invalid_params(id, "missing 'id'"),
            };
            let message_id = match mycelium::message::MessageId::from_hex(msg_id_str.as_bytes()) {
                Ok(mid) => mid,
                Err(_) => return RpcResp::invalid_params(id, "invalid message id"),
            };
            match state.node.lock().await.message_status(message_id) {
                Some(info) => match serde_json::to_value(info) {
                    Ok(v) => RpcResp::ok(id, v),
                    Err(e) => RpcResp::err(id, -32603, &e.to_string()),
                },
                None => RpcResp::err(id, -32019, "message not found"),
            }
        }

        #[cfg(feature = "message")]
        "getDefaultTopicAction" => {
            let accept = state.node.lock().await.unconfigure_topic_action();
            RpcResp::ok(id, json!(accept))
        }

        #[cfg(feature = "message")]
        "setDefaultTopicAction" => {
            let accept = match &req.params {
                Some(Value::Object(o)) => o.get("accept").and_then(|v| v.as_bool()),
                Some(Value::Array(a)) => a.get(0).and_then(|v| v.as_bool()),
                _ => None,
            };
            let accept = match accept {
                Some(b) => b,
                None => return RpcResp::invalid_params(id, "missing boolean 'accept' param"),
            };
            state.node.lock().await.accept_unconfigured_topic(accept);
            RpcResp::ok(id, json!(true))
        }

        #[cfg(feature = "message")]
        "getTopics" => {
            let topics: Vec<String> = state.node.lock().await.topics()
                .into_iter()
                .map(|t| BASE64.encode(&t))
                .collect();
            RpcResp::ok(id, serde_json::to_value(topics).unwrap_or(json!([])))
        }

        #[cfg(feature = "message")]
        "addTopic" => {
            let topic_b64 = match str_param(&req.params, 0, "topic") {
                Some(s) => s,
                None => return RpcResp::invalid_params(id, "missing 'topic'"),
            };
            let topic_bytes = match BASE64.decode(topic_b64.as_bytes()) {
                Ok(b) => b,
                Err(_) => return RpcResp::invalid_params(id, "topic: invalid base64"),
            };
            state.node.lock().await.add_topic_whitelist(topic_bytes);
            RpcResp::ok(id, json!(true))
        }

        #[cfg(feature = "message")]
        "removeTopic" => {
            let topic_b64 = match str_param(&req.params, 0, "topic") {
                Some(s) => s,
                None => return RpcResp::invalid_params(id, "missing 'topic'"),
            };
            let topic_bytes = match BASE64.decode(topic_b64.as_bytes()) {
                Ok(b) => b,
                Err(_) => return RpcResp::invalid_params(id, "topic: invalid base64"),
            };
            state.node.lock().await.remove_topic_whitelist(topic_bytes);
            RpcResp::ok(id, json!(true))
        }

        #[cfg(feature = "message")]
        "getTopicSources" => {
            let topic_b64 = match str_param(&req.params, 0, "topic") {
                Some(s) => s,
                None => return RpcResp::invalid_params(id, "missing 'topic'"),
            };
            let topic_bytes = match BASE64.decode(topic_b64.as_bytes()) {
                Ok(b) => b,
                Err(_) => return RpcResp::invalid_params(id, "topic: invalid base64"),
            };
            let subnets: Vec<String> = match state.node.lock().await.topic_allowed_sources(&topic_bytes) {
                Some(s) => s.into_iter().map(|sub| sub.to_string()).collect(),
                None => return RpcResp::err(id, -32030, "topic not found"),
            };
            RpcResp::ok(id, serde_json::to_value(subnets).unwrap_or(json!([])))
        }

        #[cfg(feature = "message")]
        "addTopicSource" => {
            let topic_b64 = match str_param(&req.params, 0, "topic") {
                Some(s) => s,
                None => return RpcResp::invalid_params(id, "missing 'topic'"),
            };
            let subnet_str = match str_param(&req.params, 1, "subnet") {
                Some(s) => s,
                None => return RpcResp::invalid_params(id, "missing 'subnet'"),
            };
            let topic_bytes = match BASE64.decode(topic_b64.as_bytes()) {
                Ok(b) => b,
                Err(_) => return RpcResp::invalid_params(id, "topic: invalid base64"),
            };
            let subnet: Subnet = match subnet_str.parse() {
                Ok(s) => s,
                Err(_) => return RpcResp::invalid_params(id, "invalid subnet"),
            };
            state.node.lock().await.add_topic_whitelist_src(topic_bytes, subnet);
            RpcResp::ok(id, json!(true))
        }

        #[cfg(feature = "message")]
        "removeTopicSource" => {
            let topic_b64 = match str_param(&req.params, 0, "topic") {
                Some(s) => s,
                None => return RpcResp::invalid_params(id, "missing 'topic'"),
            };
            let subnet_str = match str_param(&req.params, 1, "subnet") {
                Some(s) => s,
                None => return RpcResp::invalid_params(id, "missing 'subnet'"),
            };
            let topic_bytes = match BASE64.decode(topic_b64.as_bytes()) {
                Ok(b) => b,
                Err(_) => return RpcResp::invalid_params(id, "topic: invalid base64"),
            };
            let subnet: Subnet = match subnet_str.parse() {
                Ok(s) => s,
                Err(_) => return RpcResp::invalid_params(id, "invalid subnet"),
            };
            state.node.lock().await.remove_topic_whitelist_src(topic_bytes, subnet);
            RpcResp::ok(id, json!(true))
        }

        #[cfg(feature = "message")]
        "getTopicForwardSocket" => {
            let topic_b64 = match str_param(&req.params, 0, "topic") {
                Some(s) => s,
                None => return RpcResp::invalid_params(id, "missing 'topic'"),
            };
            let topic_bytes = match BASE64.decode(topic_b64.as_bytes()) {
                Ok(b) => b,
                Err(_) => return RpcResp::invalid_params(id, "topic: invalid base64"),
            };
            let node = state.node.lock().await;
            let path = node.get_topic_forward_socket(&topic_bytes)
                .map(|p| p.to_string_lossy().to_string());
            RpcResp::ok(id, serde_json::to_value(path).unwrap_or(Value::Null))
        }

        #[cfg(feature = "message")]
        "setTopicForwardSocket" => {
            let topic_b64 = match str_param(&req.params, 0, "topic") {
                Some(s) => s,
                None => return RpcResp::invalid_params(id, "missing 'topic'"),
            };
            let socket_path_str = match str_param(&req.params, 1, "socket_path") {
                Some(s) => s,
                None => return RpcResp::invalid_params(id, "missing 'socket_path'"),
            };
            let topic_bytes = match BASE64.decode(topic_b64.as_bytes()) {
                Ok(b) => b,
                Err(_) => return RpcResp::invalid_params(id, "topic: invalid base64"),
            };
            state.node.lock().await.set_topic_forward_socket(topic_bytes, std::path::PathBuf::from(socket_path_str));
            RpcResp::ok(id, json!(true))
        }

        #[cfg(feature = "message")]
        "removeTopicForwardSocket" => {
            let topic_b64 = match str_param(&req.params, 0, "topic") {
                Some(s) => s,
                None => return RpcResp::invalid_params(id, "missing 'topic'"),
            };
            let topic_bytes = match BASE64.decode(topic_b64.as_bytes()) {
                Ok(b) => b,
                Err(_) => return RpcResp::invalid_params(id, "topic: invalid base64"),
            };
            state.node.lock().await.delete_topic_forward_socket(topic_bytes);
            RpcResp::ok(id, json!(true))
        }

        // ── Network (Linux) ──────────────────────────────────────────────────
        "network.getStatus" => {
            match crate::rpc::network::imp::get_status(state.managed.clone()).await {
                Ok(s) => RpcResp::ok(id, serde_json::to_value(s).unwrap_or(Value::Null)),
                Err(e) => RpcResp::err(id, e.code(), e.message()),
            }
        }

        "network.listBridges" => {
            let managed_only = opt_bool_param(&req.params, 0, "managed_only").unwrap_or(false);
            let include_addresses =
                opt_bool_param(&req.params, 1, "include_addresses").unwrap_or(false);
            let include_ports =
                opt_bool_param(&req.params, 2, "include_ports").unwrap_or(false);
            match crate::rpc::network::imp::list_bridges(
                state.managed.clone(),
                include_addresses,
                include_ports,
                managed_only,
            )
            .await
            {
                Ok(bs) => RpcResp::ok(id, json!({ "bridges": bs })),
                Err(e) => RpcResp::err(id, e.code(), e.message()),
            }
        }

        "network.ensureBridge" => {
            let name = match str_param(&req.params, 0, "name") {
                Some(s) => s,
                None => return RpcResp::invalid_params(id, "missing 'name'"),
            };
            let up = opt_bool_param(&req.params, 1, "up").unwrap_or(true);
            let mtu = opt_u32_param(&req.params, 2, "mtu");
            match crate::rpc::network::imp::ensure_bridge(state.managed.clone(), name, up, mtu)
                .await
            {
                Ok((b, created)) => RpcResp::ok(id, json!({ "bridge": b, "created": created })),
                Err(e) => RpcResp::err(id, e.code(), e.message()),
            }
        }

        "network.deleteBridge" => {
            let name = match str_param(&req.params, 0, "name") {
                Some(s) => s,
                None => return RpcResp::invalid_params(id, "missing 'name'"),
            };
            let only_if_empty =
                opt_bool_param(&req.params, 1, "only_if_empty").unwrap_or(false);
            match crate::rpc::network::imp::delete_bridge(
                state.managed.clone(),
                name,
                only_if_empty,
            )
            .await
            {
                Ok(deleted) => RpcResp::ok(id, json!({ "deleted": deleted })),
                Err(e) => RpcResp::err(id, e.code(), e.message()),
            }
        }

        "network.listAddresses" => {
            let interface = str_param(&req.params, 0, "interface");
            let bridge_only = opt_bool_param(&req.params, 1, "bridge_only").unwrap_or(false);
            let mycelium_only =
                opt_bool_param(&req.params, 2, "mycelium_only").unwrap_or(false);
            let managed_only = opt_bool_param(&req.params, 3, "managed_only").unwrap_or(false);
            match crate::rpc::network::imp::list_addresses(
                state.managed.clone(),
                interface,
                bridge_only,
                mycelium_only,
                managed_only,
            )
            .await
            {
                Ok(list) => RpcResp::ok(id, json!({ "addresses": list })),
                Err(e) => RpcResp::err(id, e.code(), e.message()),
            }
        }

        "network.addAddress" => {
            let interface = match str_param(&req.params, 0, "interface") {
                Some(s) => s,
                None => return RpcResp::invalid_params(id, "missing 'interface'"),
            };
            let address_str = match str_param(&req.params, 1, "address") {
                Some(s) => s,
                None => return RpcResp::invalid_params(id, "missing 'address'"),
            };
            let prefix_override = opt_u8_param(&req.params, 2, "prefix_len");
            let activate_listener =
                opt_bool_param(&req.params, 3, "activate_listener").unwrap_or(true);

            let (ip, parsed_prefix) =
                match crate::rpc::network::imp::parse_address_with_prefix(&address_str, None) {
                    Ok(v) => v,
                    Err(e) => return RpcResp::err(id, e.code(), e.message()),
                };
            let prefix = prefix_override.or(parsed_prefix).unwrap_or(64);

            match crate::rpc::network::imp::add_address(
                state.managed.clone(),
                interface,
                ip,
                prefix,
                activate_listener,
            )
            .await
            {
                Ok((iface, addr, plen, managed, listener_activated)) => RpcResp::ok(
                    id,
                    json!({
                        "interface": iface,
                        "address": addr,
                        "prefix_len": plen,
                        "managed": managed,
                        "listener_activated": listener_activated,
                    }),
                ),
                Err(e) => RpcResp::err(id, e.code(), e.message()),
            }
        }

        "network.removeAddress" => {
            let interface = match str_param(&req.params, 0, "interface") {
                Some(s) => s,
                None => return RpcResp::invalid_params(id, "missing 'interface'"),
            };
            let address = match str_param(&req.params, 1, "address") {
                Some(s) => s,
                None => return RpcResp::invalid_params(id, "missing 'address'"),
            };
            match crate::rpc::network::imp::remove_address(
                state.managed.clone(),
                interface,
                address,
            )
            .await
            {
                Ok((removed, listener_removed)) => RpcResp::ok(
                    id,
                    json!({ "removed": removed, "listener_removed": listener_removed }),
                ),
                Err(e) => RpcResp::err(id, e.code(), e.message()),
            }
        }

        "network.getListeners" => {
            match crate::rpc::network::imp::get_listeners(state.managed.clone()).await {
                Ok((policy, listeners)) => {
                    RpcResp::ok(id, json!({ "policy": policy, "listeners": listeners }))
                }
                Err(e) => RpcResp::err(id, e.code(), e.message()),
            }
        }

        "network.setListenerPolicy" => {
            use crate::rpc::network::models::ListenerPolicy;
            let policy_str = match str_param(&req.params, 0, "policy") {
                Some(s) => s,
                None => return RpcResp::invalid_params(id, "missing 'policy'"),
            };
            let policy = match policy_str.as_str() {
                "all_mycelium_addresses" => ListenerPolicy::AllMyceliumAddresses,
                "all_managed_addresses" => ListenerPolicy::AllManagedAddresses,
                "explicit_only" => ListenerPolicy::ExplicitOnly,
                other => {
                    return RpcResp::invalid_params(id, &format!("unknown policy: {other}"))
                }
            };
            let explicit = opt_str_vec_param(&req.params, 1, "explicit_addresses");
            match crate::rpc::network::imp::set_listener_policy(
                state.managed.clone(),
                policy,
                explicit,
            )
            .await
            {
                Ok(p) => RpcResp::ok(id, json!({ "policy": p, "listeners_reloaded": true })),
                Err(e) => RpcResp::err(id, e.code(), e.message()),
            }
        }

        _ => RpcResp::method_not_found(id),
    }
}
