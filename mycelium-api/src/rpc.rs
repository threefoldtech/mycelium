//! JSON-RPC API implementation for Mycelium

mod spec;

use std::net::SocketAddr;
use std::sync::Arc;
use std::str::FromStr;
use std::ops::Deref;

use jsonrpc_core::{IoHandler, Error, ErrorCode, Params, Value};
use jsonrpc_http_server::tokio::runtime::TaskExecutor;
use jsonrpc_http_server::{Server, ServerBuilder};
use serde_json::json;
use tokio::sync::Mutex;
use tracing::{debug, error};
use base64::Engine;

use crate::{ServerState, Info, Route, QueriedSubnet, NoRouteSubnet, Metric};
use mycelium::metrics::Metrics;
use mycelium::endpoint::Endpoint;
use mycelium::peer_manager::{PeerExists, PeerNotFound};
use mycelium::crypto::PublicKey;

use self::spec::OPENRPC_SPEC;

/// JSON-RPC API server handle. The server is spawned in a background task. If this handle is dropped,
/// the server is terminated.
pub struct JsonRpc {
    /// JSON-RPC server handle
    _server: Server,
}

impl JsonRpc {
    /// Spawns a new JSON-RPC API server on the provided listening address.
    ///
    /// # Arguments
    ///
    /// * `node` - The Mycelium node to use for the JSON-RPC API
    /// * `listen_addr` - The address to listen on for JSON-RPC requests
    ///
    /// # Returns
    ///
    /// A `JsonRpc` instance that will be dropped when the server is terminated
    pub fn spawn<M>(node: Arc<Mutex<mycelium::Node<M>>>, listen_addr: SocketAddr) -> Self
    where
        M: Metrics + Clone + Send + Sync + 'static,
    {
        debug!(%listen_addr, "Starting JSON-RPC server");
        
        let server_state = crate::ServerState {
            node,
        };
        
        let mut io = IoHandler::new();
        
        // Register Admin methods
        {
            let state = server_state.clone();
            io.add_method("getInfo", move |_params| {
                let info = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let node_info = state.node.lock().await.info();
                        Info {
                            node_subnet: node_info.node_subnet.to_string(),
                            node_pubkey: node_info.node_pubkey,
                        }
                    })
                });
                
                Ok(serde_json::to_value(info).expect("Info struct can be encoded"))
            });
        }
        
        {
            let state = server_state.clone();
            io.add_method("getPublicKeyFromIp", move |params: Params| {
                let ip_str = params.parse::<String>()?;
                let ip = std::net::IpAddr::from_str(&ip_str).map_err(|e| Error {
                    code: ErrorCode::InvalidParams,
                    message: format!("Invalid IP address: {}", e),
                    data: None,
                })?;
                
                let pubkey = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        state.node.lock().await.get_pubkey_from_ip(ip)
                    })
                });
                
                match pubkey {
                    Some(pk) => Ok(serde_json::to_value(pk).expect("Public key can be encoded")),
                    None => Err(Error {
                        code: ErrorCode::ServerError(404),
                        message: "Public key not found".to_string(),
                        data: None,
                    }),
                }
            });
        }
        
        // Register Peer methods
        {
            let state = server_state.clone();
            io.add_method("getPeers", move |_params| {
                let peers = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        state.node.lock().await.peer_info()
                    })
                });
                
                Ok(serde_json::to_value(peers).expect("Peer info can be encoded"))
            });
        }
        
        {
            let state = server_state.clone();
            io.add_method("addPeer", move |params: Params| {
                let endpoint_str = params.parse::<String>()?;
                let endpoint = Endpoint::from_str(&endpoint_str).map_err(|e| Error {
                    code: ErrorCode::InvalidParams,
                    message: e.to_string(),
                    data: None,
                })?;
                
                let result = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        state.node.lock().await.add_peer(endpoint)
                    })
                });
                
                match result {
                    Ok(()) => Ok(json!(true)),
                    Err(PeerExists) => Err(Error {
                        code: ErrorCode::ServerError(409),
                        message: "A peer identified by that endpoint already exists".to_string(),
                        data: None,
                    }),
                }
            });
        }
        
        {
            let state = server_state.clone();
            io.add_method("deletePeer", move |params: Params| {
                let endpoint_str = params.parse::<String>()?;
                let endpoint = Endpoint::from_str(&endpoint_str).map_err(|e| Error {
                    code: ErrorCode::InvalidParams,
                    message: e.to_string(),
                    data: None,
                })?;
                
                let result = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        state.node.lock().await.remove_peer(endpoint)
                    })
                });
                
                match result {
                    Ok(()) => Ok(json!(true)),
                    Err(PeerNotFound) => Err(Error {
                        code: ErrorCode::ServerError(404),
                        message: "A peer identified by that endpoint does not exist".to_string(),
                        data: None,
                    }),
                }
            });
        }
        
        // Register Route methods
        {
            let state = server_state.clone();
            io.add_method("getSelectedRoutes", move |_params| {
                let routes = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        state.node.lock().await.selected_routes()
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
                            .collect::<Vec<_>>()
                    })
                });
                
                Ok(serde_json::to_value(routes).expect("Can encode selected routes"))
            });
        }
        
        {
            let state = server_state.clone();
            io.add_method("getFallbackRoutes", move |_params| {
                let routes = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        state.node.lock().await.fallback_routes()
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
                            .collect::<Vec<_>>()
                    })
                });
                
                Ok(serde_json::to_value(routes).expect("Can encode fallback routes"))
            });
        }
        
        {
            let state = server_state.clone();
            io.add_method("getQueriedSubnets", move |_params| {
                let subnets = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        state.node.lock().await.queried_subnets()
                            .into_iter()
                            .map(|qs| QueriedSubnet {
                                subnet: qs.subnet().to_string(),
                                expiration: qs
                                    .query_expires()
                                    .duration_since(tokio::time::Instant::now())
                                    .as_secs()
                                    .to_string(),
                            })
                            .collect::<Vec<_>>()
                    })
                });
                
                Ok(serde_json::to_value(subnets).expect("Can encode queried subnets"))
            });
        }
        
        {
            let state = server_state.clone();
            io.add_method("getNoRouteEntries", move |_params| {
                let entries = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        state.node.lock().await.no_route_entries()
                            .into_iter()
                            .map(|nrs| NoRouteSubnet {
                                subnet: nrs.subnet().to_string(),
                                expiration: nrs
                                    .entry_expires()
                                    .duration_since(tokio::time::Instant::now())
                                    .as_secs()
                                    .to_string(),
                            })
                            .collect::<Vec<_>>()
                    })
                });
                
                Ok(serde_json::to_value(entries).expect("Can encode no route entries"))
            });
        }
        
        // Register Message methods if the feature is enabled
        #[cfg(feature = "message")]
        {
            let state = server_state.clone();
            io.add_method("popMessage", move |params: Params| {
                #[derive(serde::Deserialize)]
                struct PopMessageParams {
                    peek: Option<bool>,
                    timeout: Option<u64>,
                    topic: Option<String>,
                }
                
                let params: PopMessageParams = params.parse()?;
                
                let topic_bytes = if let Some(topic_str) = params.topic {
                    Some(base64::engine::general_purpose::STANDARD.decode(topic_str.as_bytes())
                        .map_err(|e| Error {
                            code: ErrorCode::InvalidParams,
                            message: format!("Invalid base64 encoding: {}", e),
                            data: None,
                        })?)
                } else {
                    None
                };
                
                // A timeout of 0 seconds essentially means get a message if there is one, and return
                // immediately if there isn't.
                let result = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        tokio::time::timeout(
                            std::time::Duration::from_secs(params.timeout.unwrap_or(0)),
                            state.node.lock().await.get_message(!params.peek.unwrap_or(false), topic_bytes),
                        ).await
                    })
                });
                
                match result {
                    Ok(m) => {
                        let message_info = crate::message::MessageReceiveInfo {
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
                        };
                        Ok(serde_json::to_value(message_info).expect("Can encode message info"))
                    },
                    _ => Err(Error {
                        code: ErrorCode::ServerError(404),
                        message: "No message ready".to_string(),
                        data: None,
                    }),
                }
            });
            
            let state = server_state.clone();
            io.add_method("pushMessage", move |params: Params| {
                #[derive(serde::Deserialize)]
                struct PushMessageParams {
                    destination: String,
                    payload: String,
                    topic: Option<String>,
                    reply_timeout: Option<u64>,
                }
                
                let params: PushMessageParams = params.parse()?;
                
                // Parse the destination
                let dst = if params.destination.starts_with("0x") {
                    // It's a public key
                    let pk = mycelium::crypto::PublicKey::try_from(params.destination.as_str())
                        .map_err(|_| Error {
                            code: ErrorCode::InvalidParams,
                            message: "Invalid public key".to_string(),
                            data: None,
                        })?;
                    crate::message::MessageDestination::Pk(pk)
                } else {
                    // It's an IP address
                    let ip = std::net::IpAddr::from_str(&params.destination)
                        .map_err(|e| Error {
                            code: ErrorCode::InvalidParams,
                            message: format!("Invalid IP address: {}", e),
                            data: None,
                        })?;
                    crate::message::MessageDestination::Ip(ip)
                };
                
                // Decode the payload from base64
                let payload = base64::engine::general_purpose::STANDARD.decode(params.payload.as_bytes())
                    .map_err(|e| Error {
                        code: ErrorCode::InvalidParams,
                        message: format!("Invalid base64 encoding for payload: {}", e),
                        data: None,
                    })?;
                
                // Decode the topic from base64 if present
                let topic = if let Some(topic_str) = params.topic {
                    Some(base64::engine::general_purpose::STANDARD.decode(topic_str.as_bytes())
                        .map_err(|e| Error {
                            code: ErrorCode::InvalidParams,
                            message: format!("Invalid base64 encoding for topic: {}", e),
                            data: None,
                        })?)
                } else {
                    None
                };
                
                // Default message try duration
                const DEFAULT_MESSAGE_TRY_DURATION: std::time::Duration = std::time::Duration::from_secs(60 * 5);
                
                // Create the message info
                let message_info = crate::message::MessageSendInfo {
                    dst,
                    payload,
                    topic,
                };
                
                // Push the message
                let result = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let dst = match message_info.dst {
                            crate::message::MessageDestination::Ip(ip) => ip,
                            crate::message::MessageDestination::Pk(pk) => pk.address().into(),
                        };
                        
                        state.node.lock().await.push_message(
                            dst,
                            message_info.payload,
                            message_info.topic,
                            DEFAULT_MESSAGE_TRY_DURATION,
                            params.reply_timeout.is_some(),
                        )
                    })
                });
                
                let (id, sub) = match result {
                    Ok((id, sub)) => (id, sub),
                    Err(_) => {
                        return Err(Error {
                            code: ErrorCode::InvalidParams,
                            message: "Failed to push message".to_string(),
                            data: None,
                        });
                        
                        
                    }
                };
                
                if params.reply_timeout.is_none() {
                    // If we don't wait for the reply just return here.
                    return Ok(Value::from(serde_json::to_value(crate::message::MessageIdReply { id }).unwrap()));
                }
                
                let mut sub = sub.unwrap();
                
                // Wait for reply with timeout
                let reply_result = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        tokio::select! {
                            sub_res = sub.changed() => {
                                match sub_res {
                                    Ok(_) => {
                                        if let Some(m) = sub.borrow().deref() {
                                            let reply = crate::message::MessageReceiveInfo {
                                                id: m.id,
                                                src_ip: m.src_ip,
                                                src_pk: m.src_pk,
                                                dst_ip: m.dst_ip,
                                                dst_pk: m.dst_pk,
                                                topic: if m.topic.is_empty() { None } else { Some(m.topic.clone()) },
                                                payload: m.data.clone(),
                                            };
                                            Ok(serde_json::to_value(reply).expect("Can encode message receive info"))
                                        } else {
                                            // This happens if a none value is send, which should not happen.
                                            Err(Error {
                                                code: ErrorCode::InternalError,
                                                message: "Internal error while waiting for reply".to_string(),
                                                data: None,
                                            })
                                        }
                                    }
                                    Err(_) => {
                                        // This happens if the sender drops, which should not happen.
                                        Err(Error {
                                            code: ErrorCode::InternalError,
                                            message: "Internal error while waiting for reply".to_string(),
                                            data: None,
                                        })
                                    }
                                }
                            },
                            _ = tokio::time::sleep(std::time::Duration::from_secs(params.reply_timeout.unwrap_or(0))) => {
                                // Timeout expired while waiting for reply
                                Ok(serde_json::to_value(crate::message::MessageIdReply { id }).expect("Can encode message id reply"))
                            }
                        }
                    })
                });
                
                match reply_result {
                    Ok(response) => Ok(response),
                    Err(e) => Err(e),
                }
            });

            let state = server_state.clone();
            io.add_method("getMessageInfo", move |params: Params| {
                let id: String = params.parse()?;
                
                // Parse the message ID
                let message_id = mycelium::message::MessageId::from_hex(id.as_bytes())
                    .map_err(|_| Error {
                        code: ErrorCode::InvalidParams,
                        message: "Invalid message ID".to_string(),
                        data: None,
                    })?;
                
                // Get the message status
                let result = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        state.node.lock().await.message_status(message_id)
                    })
                });
                
                match result {
                    Some(info) => Ok(Value::from(serde_json::to_value(info).unwrap())),
                    None => Err(Error {
                        code: ErrorCode::ServerError(404),
                        message: "Message not found".to_string(),
                        data: None,
                    }),
                }
            });


            let state = server_state.clone();
            io.add_method("pushMessageReply", move |params: Params| {
                #[derive(serde::Deserialize)]
                struct PushMessageReplyParams {
                    id: String,
                    destination: String,
                    payload: String,
                    topic: Option<String>,
                }
                
                let params: PushMessageReplyParams = params.parse()?;
                
                // Parse the message ID
                let message_id = mycelium::message::MessageId::from_hex(params.id.as_bytes())
                    .map_err(|_| Error {
                        code: ErrorCode::InvalidParams,
                        message: "Invalid message ID".to_string(),
                        data: None,
                    })?;
                
                // Parse the destination
                let dst = if params.destination.starts_with("0x") {
                    // It's a public key
                    let pk = mycelium::crypto::PublicKey::try_from(params.destination.as_str())
                        .map_err(|_| Error {
                            code: ErrorCode::InvalidParams,
                            message: "Invalid public key".to_string(),
                            data: None,
                        })?;
                    crate::message::MessageDestination::Pk(pk)
                } else {
                    // It's an IP address
                    let ip = std::net::IpAddr::from_str(&params.destination)
                        .map_err(|e| Error {
                            code: ErrorCode::InvalidParams,
                            message: format!("Invalid IP address: {}", e),
                            data: None,
                        })?;
                    crate::message::MessageDestination::Ip(ip)
                };
                
                // Decode the payload from base64
                let payload = base64::engine::general_purpose::STANDARD.decode(params.payload.as_bytes())
                    .map_err(|e| Error {
                        code: ErrorCode::InvalidParams,
                        message: format!("Invalid base64 encoding for payload: {}", e),
                        data: None,
                    })?;
                
                // Decode the topic from base64 if present
                let topic = if let Some(topic_str) = params.topic {
                    Some(base64::engine::general_purpose::STANDARD.decode(topic_str.as_bytes())
                        .map_err(|e| Error {
                            code: ErrorCode::InvalidParams,
                            message: format!("Invalid base64 encoding for topic: {}", e),
                            data: None,
                        })?)
                } else {
                    None
                };
                
                // Default message try duration
                const DEFAULT_MESSAGE_TRY_DURATION: std::time::Duration = std::time::Duration::from_secs(60 * 5);
                
                // Create the message info
                let message_info = crate::message::MessageSendInfo {
                    dst,
                    payload,
                    topic,
                };
                
                // Reply to the message
                tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(async {
                        let dst = match message_info.dst {
                            crate::message::MessageDestination::Ip(ip) => ip,
                            crate::message::MessageDestination::Pk(pk) => pk.address().into(),
                        };
                        
                        state.node.lock().await.reply_message(
                            message_id,
                            dst,
                            message_info.payload,
                            DEFAULT_MESSAGE_TRY_DURATION,
                        );
                    })
                });
                
                Ok(Value::from(json!(true)))
            });
        }

        // Add method to serve the OpenRPC spec
        let spec_json: Value = serde_json::from_str(OPENRPC_SPEC)
            .expect("Failed to parse OpenRPC spec");
        io.add_method("rpc.discover", move |_params| {
            Ok(spec_json.clone())
        });
        
        // Build and start the server
        let server = ServerBuilder::new(io)
            .threads(1)
            .start_http(&listen_addr)
            .expect("Failed to start JSON-RPC server");
            
        debug!("JSON-RPC server started successfully");
        
        JsonRpc { _server: server }
    }
    
    /// Creates a new JSON-RPC server that listens on the specified address.
    ///
    /// This method is provided for convenience when the Node instance is not available
    /// at the time of creating the JSON-RPC server. The user is responsible for
    /// setting up the server with the appropriate Node instance later.
    ///
    /// # Arguments
    ///
    /// * `listen_addr` - The address to listen on for JSON-RPC requests
    ///
    /// # Returns
    ///
    /// A `JsonRpc` instance that will be dropped when the server is terminated
    pub fn spawn_with_addr(listen_addr: SocketAddr) -> Self {
        debug!("Starting JSON-RPC server on {}", listen_addr);
        
        let mut io = IoHandler::new();
        
        // Add method to serve the OpenRPC spec
        let spec_json: Value = serde_json::from_str(OPENRPC_SPEC)
            .expect("Failed to parse OpenRPC spec");
        io.add_method("rpc.discover", move |_params| {
            Ok(spec_json.clone())
        });
        
        // Build and start the server
        let server = ServerBuilder::new(io)
            .threads(4)
            .start_http(&listen_addr)
            .expect("Failed to start JSON-RPC server");
            
        debug!("JSON-RPC server started successfully (spec only)");
        
        JsonRpc { _server: server }
    }
}
