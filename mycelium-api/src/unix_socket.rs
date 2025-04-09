
use std::{
    path::Path,
    sync::Arc,
    ops::Deref,
};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_ENGINE;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::Mutex,
};
use tracing::{debug, error, info};

use mycelium::metrics::Metrics;

use crate::{
    HttpServerState,
    message::{
        MessageDestination, MessageReceiveInfo, MessageSendInfo,
    },
};

/// JSON-RPC 2.0 request structure
#[derive(Deserialize, Debug)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

/// JSON-RPC 2.0 response structure
#[derive(Serialize, Debug)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

/// JSON-RPC 2.0 error structure
#[derive(Serialize, Debug)]
struct JsonRpcError {
    code: i32,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

/// Unix socket server handle
pub struct UnixSocketServer {
    /// Channel to send cancellation to the unix socket server
    pub(crate) _cancel_tx: tokio::sync::oneshot::Sender<()>,
}

impl UnixSocketServer {
    /// Spawns a new Unix socket server for OpenRPC
    pub fn spawn<M>(node: mycelium::Node<M>, socket_path: impl AsRef<Path>) -> Self
    where
        M: Metrics + Clone + Send + Sync + 'static,
    {
        let server_state = HttpServerState {
            node: Arc::new(Mutex::new(node)),
        };
        
        let socket_path = socket_path.as_ref().to_path_buf();
        
        // Remove the socket file if it already exists
        if socket_path.exists() {
            if let Err(e) = std::fs::remove_file(&socket_path) {
                error!(err=%e, "Failed to remove existing socket file");
            }
        }
        
        let (_cancel_tx, mut cancel_rx) = tokio::sync::oneshot::channel();
        
        tokio::spawn(async move {
            let listener = match UnixListener::bind(&socket_path) {
                Ok(listener) => listener,
                Err(e) => {
                    error!(err=%e, "Failed to bind Unix socket");
                    error!("Unix socket OpenRPC API disabled");
                    return;
                }
            };
            
            info!("Unix socket OpenRPC server listening on {}", socket_path.display());
            
            loop {
                tokio::select! {
                    _ = &mut cancel_rx => {
                        info!("Unix socket OpenRPC server shutting down");
                        break;
                    }
                    stream_result = listener.accept() => {
                        match stream_result {
                            Ok((stream, _addr)) => {
                                let state = server_state.clone();
                                tokio::spawn(async move {
                                    handle_connection(stream, state).await;
                                });
                            }
                            Err(e) => {
                                error!(err=%e, "Failed to accept Unix socket connection");
                            }
                        }
                    }
                }
            }
            
            // Clean up the socket file when shutting down
            if let Err(e) = std::fs::remove_file(&socket_path) {
                error!(err=%e, "Failed to remove socket file during shutdown");
            }
        });
        
        UnixSocketServer { _cancel_tx }
    }
}

/// Handle a single Unix socket connection
async fn handle_connection<M>(mut stream: UnixStream, state: HttpServerState<M>)
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    // Read the request
    let mut buffer = Vec::new();
    match stream.read_to_end(&mut buffer).await {
        Ok(_) => {
            debug!("Read {} bytes from Unix socket", buffer.len());
        }
        Err(e) => {
            error!(err=%e, "Failed to read from Unix socket");
            return;
        }
    }
    
    // Parse the request
    let request: JsonRpcRequest = match serde_json::from_slice(&buffer) {
        Ok(req) => req,
        Err(e) => {
            error!(err=%e, "Failed to parse JSON-RPC request");
            let response = JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: Value::Null,
                result: None,
                error: Some(JsonRpcError {
                    code: -32700,
                    message: "Parse error".to_string(),
                    data: Some(json!(format!("{}", e))),
                }),
            };
            
            if let Err(e) = send_response(&mut stream, response).await {
                error!(err=%e, "Failed to send error response");
            }
            return;
        }
    };
    
    debug!(method=%request.method, "Processing OpenRPC request");
    
    // Process the request
    let response = process_request(request, state).await;
    
    // Send the response
    if let Err(e) = send_response(&mut stream, response).await {
        error!(err=%e, "Failed to send response");
    }
    
    // The connection will be closed when the stream is dropped
}

/// Send a JSON-RPC response
async fn send_response(stream: &mut UnixStream, response: JsonRpcResponse) -> std::io::Result<()> {
    let response_json = serde_json::to_vec(&response)?;
    stream.write_all(&response_json).await?;
    stream.flush().await?;
    Ok(())
}

/// Process a JSON-RPC request and generate a response
async fn process_request<M>(request: JsonRpcRequest, state: HttpServerState<M>) -> JsonRpcResponse
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    // Ensure we're using JSON-RPC 2.0
    if request.jsonrpc != "2.0" {
        return JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: request.id,
            result: None,
            error: Some(JsonRpcError {
                code: -32600,
                message: "Invalid Request: only JSON-RPC 2.0 is supported".to_string(),
                data: None,
            }),
        };
    }
    
    // Dispatch to the appropriate method handler
    match request.method.as_str() {
        "getInfo" => handle_get_info(request.id, state).await,
        "getPeers" => handle_get_peers(request.id, state).await,
        "addPeer" => handle_add_peer(request.id, request.params, state).await,
        "deletePeer" => handle_delete_peer(request.id, request.params, state).await,
        "getSelectedRoutes" => handle_get_selected_routes(request.id, state).await,
        "getFallbackRoutes" => handle_get_fallback_routes(request.id, state).await,
        "getPubkeyFromIp" => handle_get_pubkey_from_ip(request.id, request.params, state).await,
        "getMessage" => handle_get_message(request.id, request.params, state).await,
        "pushMessage" => handle_push_message(request.id, request.params, state).await,
        "replyMessage" => handle_reply_message(request.id, request.params, state).await,
        "messageStatus" => handle_message_status(request.id, request.params, state).await,
        _ => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: request.id,
            result: None,
            error: Some(JsonRpcError {
                code: -32601,
                message: format!("Method not found: {}", request.method),
                data: None,
            }),
        },
    }
}

/// Handle getInfo method
async fn handle_get_info<M>(id: Value, state: HttpServerState<M>) -> JsonRpcResponse
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    let info = state.node.lock().await.info();
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: Some(json!({
            "nodeSubnet": info.node_subnet.to_string(),
            "nodePubkey": info.node_pubkey,
        })),
        error: None,
    }
}

/// Handle getPeers method
async fn handle_get_peers<M>(id: Value, state: HttpServerState<M>) -> JsonRpcResponse
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    let peers = state.node.lock().await.peer_info();
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: Some(serde_json::to_value(peers).unwrap_or(Value::Null)),
        error: None,
    }
}

/// Handle addPeer method
async fn handle_add_peer<M>(id: Value, params: Value, state: HttpServerState<M>) -> JsonRpcResponse
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    #[derive(Deserialize)]
    struct AddPeerParams {
        endpoint: String,
    }
    
    let params: AddPeerParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32602,
                    message: "Invalid params".to_string(),
                    data: Some(json!(e.to_string() as String)),
                }),
            };
        }
    };
    
    let endpoint = match std::str::FromStr::from_str(&params.endpoint) {
        Ok(endpoint) => endpoint,
        Err(e) => {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: None,
                error: Some(JsonRpcError {
                    code: 400,
                    message: "Invalid endpoint format".to_string(),
                    data: Some(json!(format!("{}", e))),
                }),
            };
        }
    };
    
    match state.node.lock().await.add_peer(endpoint) {
        Ok(()) => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(json!(true)),
            error: None,
        },
        Err(_) => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code: 409,
                message: "A peer identified by that endpoint already exists".to_string(),
                data: None,
            }),
        },
    }
}

/// Handle deletePeer method
async fn handle_delete_peer<M>(id: Value, params: Value, state: HttpServerState<M>) -> JsonRpcResponse
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    #[derive(Deserialize)]
    struct DeletePeerParams {
        endpoint: String,
    }
    
    let params: DeletePeerParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32602,
                    message: "Invalid params".to_string(),
                    data: Some(json!(format!("{}", e))),
                }),
            };
        }
    };
    
    let endpoint = match std::str::FromStr::from_str(&params.endpoint) {
        Ok(endpoint) => endpoint,
        Err(e) => {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: None,
                error: Some(JsonRpcError {
                    code: 400,
                    message: "Invalid endpoint format".to_string(),
                    data: Some(json!(format!("{}", e))),
                }),
            };
        }
    };
    
    match state.node.lock().await.remove_peer(endpoint) {
        Ok(()) => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(json!(true)),
            error: None,
        },
        Err(_) => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code: 404,
                message: "A peer identified by that endpoint does not exist".to_string(),
                data: None,
            }),
        },
    }
}

/// Handle getSelectedRoutes method
async fn handle_get_selected_routes<M>(id: Value, state: HttpServerState<M>) -> JsonRpcResponse
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    let routes = state
        .node
        .lock()
        .await
        .selected_routes()
        .into_iter()
        .map(|sr| {
            let metric = if sr.metric().is_infinite() {
                json!("infinite")
            } else {
                json!(u16::from(sr.metric()))
            };
            
            json!({
                "subnet": sr.source().subnet().to_string(),
                "nextHop": sr.neighbour().connection_identifier().clone(),
                "metric": metric,
                "seqno": u16::from(sr.seqno()),
            })
        })
        .collect::<Vec<_>>();
    
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: Some(json!(routes)),
        error: None,
    }
}

/// Handle getFallbackRoutes method
async fn handle_get_fallback_routes<M>(id: Value, state: HttpServerState<M>) -> JsonRpcResponse
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    let routes = state
        .node
        .lock()
        .await
        .fallback_routes()
        .into_iter()
        .map(|sr| {
            let metric = if sr.metric().is_infinite() {
                json!("infinite")
            } else {
                json!(u16::from(sr.metric()))
            };
            
            json!({
                "subnet": sr.source().subnet().to_string(),
                "nextHop": sr.neighbour().connection_identifier().clone(),
                "metric": metric,
                "seqno": u16::from(sr.seqno()),
            })
        })
        .collect::<Vec<_>>();
    
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: Some(json!(routes)),
        error: None,
    }
}

/// Handle getPubkeyFromIp method
async fn handle_get_pubkey_from_ip<M>(id: Value, params: Value, state: HttpServerState<M>) -> JsonRpcResponse
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    #[derive(Deserialize)]
    struct GetPubkeyParams {
        ip: String,
    }
    
    let params: GetPubkeyParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32602,
                    message: "Invalid params".to_string(),
                    data: Some(json!(format!("{}", e))),
                }),
            };
        }
    };
    
    let ip = match params.ip.parse() {
        Ok(ip) => ip,
        Err(e) => {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: None,
                error: Some(JsonRpcError {
                    code: 400,
                    message: "Invalid IP format".to_string(),
                    data: Some(json!(format!("{}", e))),
                }),
            };
        }
    };
    
    match state.node.lock().await.get_pubkey_from_ip(ip) {
        Some(pubkey) => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(json!({
                "publicKey": pubkey,
            })),
            error: None,
        },
        None => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code: 404,
                message: "No public key found for the given IP".to_string(),
                data: None,
            }),
        },
    }
}

/// Handle getMessage method
async fn handle_get_message<M>(id: Value, params: Value, state: HttpServerState<M>) -> JsonRpcResponse
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    #[derive(Deserialize)]
    struct GetMessageParams {
        #[serde(default)]
        peek: bool,
        #[serde(default)]
        timeout: u64,
        #[serde(default)]
        topic: Option<String>,
    }
    
    let params: GetMessageParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32602,
                    message: "Invalid params".to_string(),
                    data: Some(json!(format!("{}", e))),
                }),
            };
        }
    };
    
    let topic = params.topic.map(|t| {
        BASE64_ENGINE.decode(&t).unwrap_or_default()
    });
    
    // A timeout of 0 seconds essentially means get a message if there is one, and return
    // immediately if there isn't.
    match tokio::time::timeout(
        std::time::Duration::from_secs(params.timeout),
        state.node.lock().await.get_message(!params.peek, topic),
    ).await {
        Ok(message) => {
            let result = MessageReceiveInfo {
                id: message.id,
                src_ip: message.src_ip,
                src_pk: message.src_pk,
                dst_ip: message.dst_ip,
                dst_pk: message.dst_pk,
                topic: if message.topic.is_empty() {
                    None
                } else {
                    Some(message.topic)
                },
                payload: message.data,
            };
            
            JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: Some(serde_json::to_value(result).unwrap_or(Value::Null)),
                error: None,
            }
        },
        Err(_) => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code: 204,
                message: "No message available".to_string(),
                data: None,
            }),
        },
    }
}

/// Handle pushMessage method
async fn handle_push_message<M>(id: Value, params: Value, state: HttpServerState<M>) -> JsonRpcResponse
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    #[derive(Deserialize)]
    struct PushMessageParams {
        #[serde(default)]
        reply_timeout: Option<u64>,
        message: MessageSendInfo,
    }
    
    let params: PushMessageParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32602,
                    message: "Invalid params".to_string(),
                    data: Some(json!(e.to_string() as String)),
                }),
            };
        }
    };
    
    let dst = match &params.message.dst {
        MessageDestination::Ip(ip) => *ip,
        MessageDestination::Pk(pk) => std::net::IpAddr::V6(pk.address()),
    };
    
    // Default message try duration
    let try_duration = std::time::Duration::from_secs(60 * 5);
    
    let await_reply = params.reply_timeout.is_some();
    
    match state.node.lock().await.push_message(
        dst,
        params.message.payload,
        params.message.topic,
        try_duration,
        await_reply,
    ) {
        Ok((id, sub)) => {
            if !await_reply {
                // If we don't wait for the reply just return here
                return JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id: serde_json::to_value(id.as_hex()).unwrap(),
                    result: Some(json!({
                        "id": id.as_hex(),
                    })),
                    error: None,
                };
            }
            
            let mut sub = sub.unwrap();
            let timeout = params.reply_timeout.unwrap_or(0);
            
            tokio::select! {
                sub_res = sub.changed() => {
                    match sub_res {
                        Ok(_) => {
                            if let Some(m) = sub.borrow().deref() {
                                let result = MessageReceiveInfo {
                                    id: m.id,
                                    src_ip: m.src_ip,
                                    src_pk: m.src_pk,
                                    dst_ip: m.dst_ip,
                                    dst_pk: m.dst_pk,
                                    topic: if m.topic.is_empty() { None } else { Some(m.topic.clone()) },
                                    payload: m.data.clone(),
                                };
                                
                                JsonRpcResponse {
                                    jsonrpc: "2.0".to_string(),
                                    id: serde_json::to_value(id.as_hex()).unwrap(),
                                    result: Some(serde_json::to_value(result).unwrap_or(Value::Null)),
                                    error: None,
                                }
                            } else {
                                // This happens if a none value is sent, which should not happen
                                JsonRpcResponse {
                                    jsonrpc: "2.0".to_string(),
                                    id: serde_json::to_value(id.as_hex()).unwrap(),
                                    result: None,
                                    error: Some(JsonRpcError {
                                        code: 500,
                                        message: "Internal server error".to_string(),
                                        data: None,
                                    }),
                                }
                            }
                        }
                        Err(_) => {
                            // This happens if the sender drops, which should not happen
                            JsonRpcResponse {
                                jsonrpc: "2.0".to_string(),
                                id: serde_json::to_value(id.as_hex()).unwrap(),
                                result: None,
                                error: Some(JsonRpcError {
                                    code: 500,
                                    message: "Internal server error".to_string(),
                                    data: None,
                                }),
                            }
                        }
                    }
                },
                _ = tokio::time::sleep(std::time::Duration::from_secs(timeout)) => {
                    // Timeout expired while waiting for reply
                    JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id: serde_json::to_value(id.as_hex()).unwrap(),
                        result: None,
                        error: Some(JsonRpcError {
                            code: 408,
                            message: "Timeout waiting for reply".to_string(),
                            data: None,
                        }),
                    }
                }
            }
        },
        Err(_) => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code: 400,
                message: "Invalid message format or destination".to_string(),
                data: None,
            }),
        },
    }
}

/// Handle replyMessage method
async fn handle_reply_message<M>(id: Value, params: Value, state: HttpServerState<M>) -> JsonRpcResponse
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    #[derive(Deserialize)]
    struct ReplyMessageParams {
        id: String,
        message: MessageSendInfo,
    }
    
    let params: ReplyMessageParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32602,
                    message: "Invalid params".to_string(),
                    data: Some(json!(e.to_string() as String)),
                }),
            };
        }
    };
    
    let message_id = match hex::decode(&params.id) {
        Ok(bytes) => {
            if bytes.len() != 8 {
                return JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: None,
                    error: Some(JsonRpcError {
                        code: 400,
                        message: "Invalid message ID: must be 8 bytes".to_string(),
                        data: None,
                    }),
                };
            }
            // Parse the hex string into a MessageId
            let hex_id = params.id.clone();
            if hex_id.len() != 16 {
                return JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: None,
                    error: Some(JsonRpcError {
                        code: 400,
                        message: "Invalid message ID: must be 16 characters".to_string(),
                        data: None,
                    }),
                };
            }
            
            let mut backing = [0; 8];
            match faster_hex::hex_decode(hex_id.as_bytes(), &mut backing) {
                Ok(_) => mycelium::message::MessageId::deserialize(serde_json::Value::String(hex_id)).unwrap(),
                Err(_) => {
                    return JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id,
                        result: None,
                        error: Some(JsonRpcError {
                            code: 400,
                            message: "Invalid message ID: not valid hex".to_string(),
                            data: None,
                        }),
                    };
                }
            }
        },
        Err(e) => {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: id,
                result: None,
                error: Some(JsonRpcError {
                    code: 400,
                    message: "Invalid message ID".to_string(),
                    data: Some(json!(e.to_string() as String)),
                }),
            };
        }
    };
    
    let dst = match &params.message.dst {
        MessageDestination::Ip(ip) => *ip,
        MessageDestination::Pk(pk) => std::net::IpAddr::V6(pk.address()),
    };
    
    // Default message try duration
    let try_duration = std::time::Duration::from_secs(60 * 5);
    
    // We can't directly create a MessageId, so we'll just pass the hex string
    // to the API and let it handle the conversion
    state.node.lock().await.reply_message(
        message_id,
        dst,
        params.message.payload,
        try_duration,
    );
    
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: Some(json!(true)),
        error: None,
    }
}

/// Handle messageStatus method
async fn handle_message_status<M>(id: Value, params: Value, state: HttpServerState<M>) -> JsonRpcResponse
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    #[derive(Deserialize)]
    struct MessageStatusParams {
        id: String,
    }
    
    let params: MessageStatusParams = match serde_json::from_value(params) {
        Ok(p) => p,
        Err(e) => {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id,
                result: None,
                error: Some(JsonRpcError {
                    code: -32602,
                    message: "Invalid params".to_string(),
                    data: Some(json!(format!("{}", e))),
                }),
            };
        }
    };
    
    let message_id = match hex::decode(&params.id) {
        Ok(bytes) => {
            if bytes.len() != 8 {
                return JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: None,
                    error: Some(JsonRpcError {
                        code: 400,
                        message: "Invalid message ID: must be 8 bytes".to_string(),
                        data: None,
                    }),
                };
            }
            // Parse the hex string into a MessageId
            let hex_id = params.id.clone();
            if hex_id.len() != 16 {
                return JsonRpcResponse {
                    jsonrpc: "2.0".to_string(),
                    id,
                    result: None,
                    error: Some(JsonRpcError {
                        code: 400,
                        message: "Invalid message ID: must be 16 characters".to_string(),
                        data: None,
                    }),
                };
            }
            
            let mut backing = [0; 8];
            match faster_hex::hex_decode(hex_id.as_bytes(), &mut backing) {
                Ok(_) => mycelium::message::MessageId::deserialize(serde_json::Value::String(hex_id)).unwrap(),
                Err(_) => {
                    return JsonRpcResponse {
                        jsonrpc: "2.0".to_string(),
                        id,
                        result: None,
                        error: Some(JsonRpcError {
                            code: 400,
                            message: "Invalid message ID: not valid hex".to_string(),
                            data: None,
                        }),
                    };
                }
            }
        },
        Err(e) => {
            return JsonRpcResponse {
                jsonrpc: "2.0".to_string(),
                id: id,
                result: None,
                error: Some(JsonRpcError {
                    code: 400,
                    message: "Invalid message ID".to_string(),
                    data: Some(json!(e.to_string() as String)),
                }),
            };
        }
    };
    
    // We can't directly create a MessageId, so we'll just pass the hex string
    // to the API and let it handle the conversion
    match state.node.lock().await.message_status(message_id) {
        Some(info) => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(serde_json::to_value(info).unwrap_or(Value::Null)),
            error: None,
        },
        None => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code: 404,
                message: "Message not found".to_string(),
                data: None,
            }),
        },
    }
}
