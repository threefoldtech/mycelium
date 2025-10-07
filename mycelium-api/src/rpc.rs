//! JSON-RPC API implementation for Mycelium

mod spec;

use std::net::{Ipv6Addr, SocketAddr};
#[cfg(feature = "message")]
use std::ops::Deref;
use std::str::FromStr;
use std::sync::Arc;

#[cfg(feature = "message")]
use base64::Engine;
use jsonrpsee::core::RpcResult;
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::server::{ServerBuilder, ServerHandle};
use jsonrpsee::types::{ErrorCode, ErrorObject};
#[cfg(feature = "message")]
use mycelium::subnet::Subnet;
#[cfg(feature = "message")]
use std::path::PathBuf;
use tokio::sync::Mutex;
#[cfg(feature = "message")]
use tokio::time::Duration;
use tracing::debug;

use crate::{Info, Metric, NoRouteSubnet, QueriedSubnet, Route, ServerState};
use mycelium::crypto::PublicKey;
use mycelium::endpoint::Endpoint;
use mycelium::metrics::Metrics;
use mycelium::peer_manager::{PeerExists, PeerNotFound, PeerStats};

use self::spec::OPENRPC_SPEC;

// Define the base RPC API trait using jsonrpsee macros
#[rpc(server)]
pub trait MyceliumApi {
    // Admin methods
    #[method(name = "getInfo")]
    async fn get_info(&self) -> RpcResult<Info>;

    #[method(name = "getPublicKeyFromIp")]
    async fn get_pubkey_from_ip(&self, ip: String) -> RpcResult<PublicKey>;

    // Peer methods
    #[method(name = "getPeers")]
    async fn get_peers(&self) -> RpcResult<Vec<PeerStats>>;

    #[method(name = "addPeer")]
    async fn add_peer(&self, endpoint: String) -> RpcResult<bool>;

    #[method(name = "deletePeer")]
    async fn delete_peer(&self, endpoint: String) -> RpcResult<bool>;

    // Route methods
    #[method(name = "getSelectedRoutes")]
    async fn get_selected_routes(&self) -> RpcResult<Vec<Route>>;

    #[method(name = "getFallbackRoutes")]
    async fn get_fallback_routes(&self) -> RpcResult<Vec<Route>>;

    #[method(name = "getQueriedSubnets")]
    async fn get_queried_subnets(&self) -> RpcResult<Vec<QueriedSubnet>>;

    #[method(name = "getNoRouteEntries")]
    async fn get_no_route_entries(&self) -> RpcResult<Vec<NoRouteSubnet>>;

    // Proxy methods
    #[method(name = "getProxies")]
    async fn get_proxies(&self) -> RpcResult<Vec<Ipv6Addr>>;

    #[method(name = "connectProxy")]
    async fn connect_proxy(&self, remote: Option<SocketAddr>) -> RpcResult<SocketAddr>;

    #[method(name = "disconnectProxy")]
    async fn disconnect_proxy(&self) -> RpcResult<bool>;

    #[method(name = "startProxyProbe")]
    async fn start_proxy_probe(&self) -> RpcResult<bool>;

    #[method(name = "stopProxyProbe")]
    async fn stop_proxy_probe(&self) -> RpcResult<bool>;

    // OpenRPC discovery
    #[method(name = "rpc.discover")]
    async fn discover(&self) -> RpcResult<serde_json::Value>;
}

// Define a separate message API trait that is only compiled when the message feature is enabled
#[cfg(feature = "message")]
#[rpc(server)]
pub trait MyceliumMessageApi {
    // Message methods
    #[method(name = "popMessage")]
    async fn pop_message(
        &self,
        peek: Option<bool>,
        timeout: Option<u64>,
        topic: Option<String>,
    ) -> RpcResult<Option<crate::message::MessageReceiveInfo>>;

    #[method(name = "pushMessage")]
    async fn push_message(
        &self,
        message: crate::message::MessageSendInfo,
        reply_timeout: Option<u64>,
    ) -> RpcResult<crate::message::PushMessageResponse>;

    #[method(name = "pushMessageReply")]
    async fn push_message_reply(
        &self,
        id: String,
        message: crate::message::MessageSendInfo,
    ) -> RpcResult<bool>;

    #[method(name = "getMessageInfo")]
    async fn get_message_info(&self, id: String) -> RpcResult<mycelium::message::MessageInfo>;

    // Topic configuration methods
    #[method(name = "getDefaultTopicAction")]
    async fn get_default_topic_action(&self) -> RpcResult<bool>;

    #[method(name = "setDefaultTopicAction")]
    async fn set_default_topic_action(&self, accept: bool) -> RpcResult<bool>;

    #[method(name = "getTopics")]
    async fn get_topics(&self) -> RpcResult<Vec<String>>;

    #[method(name = "addTopic")]
    async fn add_topic(&self, topic: String) -> RpcResult<bool>;

    #[method(name = "removeTopic")]
    async fn remove_topic(&self, topic: String) -> RpcResult<bool>;

    #[method(name = "getTopicSources")]
    async fn get_topic_sources(&self, topic: String) -> RpcResult<Vec<String>>;

    #[method(name = "addTopicSource")]
    async fn add_topic_source(&self, topic: String, subnet: String) -> RpcResult<bool>;

    #[method(name = "removeTopicSource")]
    async fn remove_topic_source(&self, topic: String, subnet: String) -> RpcResult<bool>;

    #[method(name = "getTopicForwardSocket")]
    async fn get_topic_forward_socket(&self, topic: String) -> RpcResult<Option<String>>;

    #[method(name = "setTopicForwardSocket")]
    async fn set_topic_forward_socket(&self, topic: String, socket_path: String)
        -> RpcResult<bool>;

    #[method(name = "removeTopicForwardSocket")]
    async fn remove_topic_forward_socket(&self, topic: String) -> RpcResult<bool>;
}

// Implement the API trait
#[derive(Clone)]
struct RPCApi<M> {
    state: Arc<ServerState<M>>,
}

// Implement the base API trait
#[async_trait::async_trait]
impl<M> MyceliumApiServer for RPCApi<M>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    async fn get_info(&self) -> RpcResult<Info> {
        debug!("Getting node info via RPC");
        let node_info = self.state.node.lock().await.info();
        Ok(Info {
            node_subnet: node_info.node_subnet.to_string(),
            node_pubkey: node_info.node_pubkey,
        })
    }

    async fn get_pubkey_from_ip(&self, ip_str: String) -> RpcResult<PublicKey> {
        debug!(ip = %ip_str, "Getting public key from IP via RPC");
        let ip = std::net::IpAddr::from_str(&ip_str)
            .map_err(|_| ErrorObject::from(ErrorCode::from(-32007)))?;

        let pubkey = self.state.node.lock().await.get_pubkey_from_ip(ip);

        match pubkey {
            Some(pk) => Ok(pk),
            None => Err(ErrorObject::from(ErrorCode::from(-32008))),
        }
    }

    async fn get_peers(&self) -> RpcResult<Vec<PeerStats>> {
        debug!("Fetching peer stats via RPC");
        let peers = self.state.node.lock().await.peer_info();
        Ok(peers)
    }

    async fn add_peer(&self, endpoint_str: String) -> RpcResult<bool> {
        debug!(
            peer.endpoint = endpoint_str,
            "Attempting to add peer to the system via RPC"
        );

        let endpoint = Endpoint::from_str(&endpoint_str)
            .map_err(|_| ErrorObject::from(ErrorCode::from(-32009)))?;

        match self.state.node.lock().await.add_peer(endpoint) {
            Ok(()) => Ok(true),
            Err(PeerExists) => Err(ErrorObject::from(ErrorCode::from(-32010))),
        }
    }

    async fn delete_peer(&self, endpoint_str: String) -> RpcResult<bool> {
        debug!(
            peer.endpoint = endpoint_str,
            "Attempting to remove peer from the system via RPC"
        );

        let endpoint = Endpoint::from_str(&endpoint_str)
            .map_err(|_| ErrorObject::from(ErrorCode::from(-32012)))?;

        match self.state.node.lock().await.remove_peer(endpoint) {
            Ok(()) => Ok(true),
            Err(PeerNotFound) => Err(ErrorObject::from(ErrorCode::from(-32011))),
        }
    }

    async fn get_selected_routes(&self) -> RpcResult<Vec<Route>> {
        debug!("Loading selected routes via RPC");
        let routes = self
            .state
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

        Ok(routes)
    }

    async fn get_fallback_routes(&self) -> RpcResult<Vec<Route>> {
        debug!("Loading fallback routes via RPC");
        let routes = self
            .state
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

        Ok(routes)
    }

    async fn get_queried_subnets(&self) -> RpcResult<Vec<QueriedSubnet>> {
        debug!("Loading queried subnets via RPC");
        let queries = self
            .state
            .node
            .lock()
            .await
            .queried_subnets()
            .into_iter()
            .map(|qs| QueriedSubnet {
                subnet: qs.subnet().to_string(),
                expiration: qs
                    .query_expires()
                    .duration_since(tokio::time::Instant::now())
                    .as_secs()
                    .to_string(),
            })
            .collect();

        Ok(queries)
    }

    async fn get_no_route_entries(&self) -> RpcResult<Vec<NoRouteSubnet>> {
        debug!("Loading no route entries via RPC");
        let entries = self
            .state
            .node
            .lock()
            .await
            .no_route_entries()
            .into_iter()
            .map(|nrs| NoRouteSubnet {
                subnet: nrs.subnet().to_string(),
                expiration: nrs
                    .entry_expires()
                    .duration_since(tokio::time::Instant::now())
                    .as_secs()
                    .to_string(),
            })
            .collect();

        Ok(entries)
    }

    async fn get_proxies(&self) -> RpcResult<Vec<Ipv6Addr>> {
        debug!("Listing known proxies via RPC");
        let proxies = self.state.node.lock().await.known_proxies();
        Ok(proxies)
    }

    async fn connect_proxy(&self, remote: Option<SocketAddr>) -> RpcResult<SocketAddr> {
        debug!(?remote, "Attempting to connect remote proxy via RPC");

        // Attempt to connect; map error to "no proxy available/valid" like HTTP 404 counterpart
        let res = self.state.node.lock().await.connect_proxy(remote).await;

        match res {
            Ok(addr) => Ok(addr),
            Err(_) => Err(ErrorObject::from(ErrorCode::from(-32032))),
        }
    }

    async fn disconnect_proxy(&self) -> RpcResult<bool> {
        debug!("Disconnecting from remote proxy via RPC");
        self.state.node.lock().await.disconnect_proxy();
        Ok(true)
    }

    async fn start_proxy_probe(&self) -> RpcResult<bool> {
        debug!("Starting proxy probe via RPC");
        self.state.node.lock().await.start_proxy_scan();
        Ok(true)
    }

    async fn stop_proxy_probe(&self) -> RpcResult<bool> {
        debug!("Stopping proxy probe via RPC");
        self.state.node.lock().await.stop_proxy_scan();
        Ok(true)
    }

    async fn discover(&self) -> RpcResult<serde_json::Value> {
        let spec = serde_json::from_str::<serde_json::Value>(OPENRPC_SPEC)
            .expect("Failed to parse OpenRPC spec");
        Ok(spec)
    }
}

// Implement the message API trait only when the message feature is enabled
#[cfg(feature = "message")]
#[async_trait::async_trait]
impl<M> MyceliumMessageApiServer for RPCApi<M>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    async fn pop_message(
        &self,
        peek: Option<bool>,
        timeout: Option<u64>,
        topic: Option<String>,
    ) -> RpcResult<Option<crate::message::MessageReceiveInfo>> {
        debug!(
            "Attempt to get message via RPC, peek {}, timeout {} seconds",
            peek.unwrap_or(false),
            timeout.unwrap_or(0)
        );

        let topic_bytes = if let Some(topic_str) = topic {
            Some(
                base64::engine::general_purpose::STANDARD
                    .decode(topic_str.as_bytes())
                    .map_err(|_| ErrorObject::from(ErrorCode::from(-32013)))?,
            )
        } else {
            None
        };

        // A timeout of 0 seconds essentially means get a message if there is one, and return
        // immediately if there isn't.
        let result = tokio::time::timeout(
            Duration::from_secs(timeout.unwrap_or(0)),
            self.state
                .node
                .lock()
                .await
                .get_message(!peek.unwrap_or(false), topic_bytes),
        )
        .await;

        match result {
            Ok(m) => Ok(Some(crate::message::MessageReceiveInfo {
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
            })),
            Err(_) => Ok(None),
        }
    }

    async fn push_message(
        &self,
        message: crate::message::MessageSendInfo,
        reply_timeout: Option<u64>,
    ) -> RpcResult<crate::message::PushMessageResponse> {
        let dst = match message.dst {
            crate::message::MessageDestination::Ip(ip) => ip,
            crate::message::MessageDestination::Pk(pk) => pk.address().into(),
        };

        debug!(
            message.dst=%dst,
            message.len=message.payload.len(),
            "Pushing new message via RPC",
        );

        // Default message try duration
        const DEFAULT_MESSAGE_TRY_DURATION: Duration = Duration::from_secs(60 * 5);

        let result = self.state.node.lock().await.push_message(
            dst,
            message.payload,
            message.topic,
            DEFAULT_MESSAGE_TRY_DURATION,
            reply_timeout.is_some(),
        );

        let (id, sub) = match result {
            Ok((id, sub)) => (id, sub),
            Err(_) => {
                return Err(ErrorObject::from(ErrorCode::from(-32015)));
            }
        };

        if reply_timeout.is_none() {
            // If we don't wait for the reply just return here.
            return Ok(crate::message::PushMessageResponse::Id(
                crate::message::MessageIdReply { id },
            ));
        }

        let mut sub = sub.unwrap();

        // Wait for reply with timeout
        tokio::select! {
            sub_res = sub.changed() => {
                match sub_res {
                    Ok(_) => {
                        if let Some(m) = sub.borrow().deref() {
                            Ok(crate::message::PushMessageResponse::Reply(crate::message::MessageReceiveInfo {
                                id: m.id,
                                src_ip: m.src_ip,
                                src_pk: m.src_pk,
                                dst_ip: m.dst_ip,
                                dst_pk: m.dst_pk,
                                topic: if m.topic.is_empty() { None } else { Some(m.topic.clone()) },
                                payload: m.data.clone(),
                            }))
                        } else {
                            // This happens if a none value is send, which should not happen.
                            Err(ErrorObject::from(ErrorCode::from(-32016)))
                        }
                    }
                    Err(_) => {
                        // This happens if the sender drops, which should not happen.
                        Err(ErrorObject::from(ErrorCode::from(-32017)))
                    }
                }
            },
            _ = tokio::time::sleep(Duration::from_secs(reply_timeout.unwrap_or(0))) => {
                // Timeout expired while waiting for reply
                Ok(crate::message::PushMessageResponse::Id(crate::message::MessageIdReply { id }))
            }
        }
    }

    async fn push_message_reply(
        &self,
        id: String,
        message: crate::message::MessageSendInfo,
    ) -> RpcResult<bool> {
        let message_id = match mycelium::message::MessageId::from_hex(id.as_bytes()) {
            Ok(id) => id,
            Err(_) => {
                return Err(ErrorObject::from(ErrorCode::from(-32018)));
            }
        };

        let dst = match message.dst {
            crate::message::MessageDestination::Ip(ip) => ip,
            crate::message::MessageDestination::Pk(pk) => pk.address().into(),
        };

        debug!(
            message.id=id,
            message.dst=%dst,
            message.len=message.payload.len(),
            "Pushing new reply to message via RPC",
        );

        // Default message try duration
        const DEFAULT_MESSAGE_TRY_DURATION: Duration = Duration::from_secs(60 * 5);

        self.state.node.lock().await.reply_message(
            message_id,
            dst,
            message.payload,
            DEFAULT_MESSAGE_TRY_DURATION,
        );

        Ok(true)
    }

    async fn get_message_info(&self, id: String) -> RpcResult<mycelium::message::MessageInfo> {
        let message_id = match mycelium::message::MessageId::from_hex(id.as_bytes()) {
            Ok(id) => id,
            Err(_) => {
                return Err(ErrorObject::from(ErrorCode::from(-32020)));
            }
        };

        debug!(message.id=%id, "Fetching message status via RPC");

        let result = self.state.node.lock().await.message_status(message_id);

        match result {
            Some(info) => Ok(info),
            None => Err(ErrorObject::from(ErrorCode::from(-32019))),
        }
    }

    // Topic configuration methods implementation
    async fn get_default_topic_action(&self) -> RpcResult<bool> {
        debug!("Getting default topic action via RPC");
        let accept = self.state.node.lock().await.unconfigure_topic_action();
        Ok(accept)
    }

    async fn set_default_topic_action(&self, accept: bool) -> RpcResult<bool> {
        debug!(accept=%accept, "Setting default topic action via RPC");
        self.state
            .node
            .lock()
            .await
            .accept_unconfigured_topic(accept);
        Ok(true)
    }

    async fn get_topics(&self) -> RpcResult<Vec<String>> {
        debug!("Getting all whitelisted topics via RPC");

        let topics = self
            .state
            .node
            .lock()
            .await
            .topics()
            .into_iter()
            .map(|topic| base64::engine::general_purpose::STANDARD.encode(&topic))
            .collect();

        // For now, we'll return an empty list
        Ok(topics)
    }

    async fn add_topic(&self, topic: String) -> RpcResult<bool> {
        debug!("Adding topic to whitelist via RPC");

        // Decode the base64 topic
        let topic_bytes = base64::engine::general_purpose::STANDARD
            .decode(topic.as_bytes())
            .map_err(|_| ErrorObject::from(ErrorCode::from(-32021)))?;

        self.state
            .node
            .lock()
            .await
            .add_topic_whitelist(topic_bytes);
        Ok(true)
    }

    async fn remove_topic(&self, topic: String) -> RpcResult<bool> {
        debug!("Removing topic from whitelist via RPC");

        // Decode the base64 topic
        let topic_bytes = base64::engine::general_purpose::STANDARD
            .decode(topic.as_bytes())
            .map_err(|_| ErrorObject::from(ErrorCode::from(-32021)))?;

        self.state
            .node
            .lock()
            .await
            .remove_topic_whitelist(topic_bytes);
        Ok(true)
    }

    async fn get_topic_sources(&self, topic: String) -> RpcResult<Vec<String>> {
        debug!("Getting sources for topic via RPC");

        // Decode the base64 topic
        let topic_bytes = base64::engine::general_purpose::STANDARD
            .decode(topic.as_bytes())
            .map_err(|_| ErrorObject::from(ErrorCode::from(-32021)))?;

        let subnets = self
            .state
            .node
            .lock()
            .await
            .topic_allowed_sources(&topic_bytes)
            .ok_or(ErrorObject::from(ErrorCode::from(-32030)))?
            .into_iter()
            .map(|subnet| subnet.to_string())
            .collect();

        Ok(subnets)
    }

    async fn add_topic_source(&self, topic: String, subnet: String) -> RpcResult<bool> {
        debug!("Adding source to topic whitelist via RPC");

        // Decode the base64 topic
        let topic_bytes = base64::engine::general_purpose::STANDARD
            .decode(topic.as_bytes())
            .map_err(|_| ErrorObject::from(ErrorCode::from(-32021)))?;

        // Parse the subnet
        let subnet_obj = subnet
            .parse::<Subnet>()
            .map_err(|_| ErrorObject::from(ErrorCode::from(-32023)))?;

        self.state
            .node
            .lock()
            .await
            .add_topic_whitelist_src(topic_bytes, subnet_obj);
        Ok(true)
    }

    async fn remove_topic_source(&self, topic: String, subnet: String) -> RpcResult<bool> {
        debug!("Removing source from topic whitelist via RPC");

        // Decode the base64 topic
        let topic_bytes = base64::engine::general_purpose::STANDARD
            .decode(topic.as_bytes())
            .map_err(|_| ErrorObject::from(ErrorCode::from(-32021)))?;

        // Parse the subnet
        let subnet_obj = subnet
            .parse::<Subnet>()
            .map_err(|_| ErrorObject::from(ErrorCode::from(-32023)))?;

        self.state
            .node
            .lock()
            .await
            .remove_topic_whitelist_src(topic_bytes, subnet_obj);
        Ok(true)
    }

    async fn get_topic_forward_socket(&self, topic: String) -> RpcResult<Option<String>> {
        debug!("Getting forward socket for topic via RPC");

        // Decode the base64 topic
        let topic_bytes = base64::engine::general_purpose::STANDARD
            .decode(topic.as_bytes())
            .map_err(|_| ErrorObject::from(ErrorCode::from(-32021)))?;

        let node = self.state.node.lock().await;
        let socket_path = node
            .get_topic_forward_socket(&topic_bytes)
            .map(|p| p.to_string_lossy().to_string());

        Ok(socket_path)
    }

    async fn set_topic_forward_socket(
        &self,
        topic: String,
        socket_path: String,
    ) -> RpcResult<bool> {
        debug!("Setting forward socket for topic via RPC");

        // Decode the base64 topic
        let topic_bytes = base64::engine::general_purpose::STANDARD
            .decode(topic.as_bytes())
            .map_err(|_| ErrorObject::from(ErrorCode::from(-32021)))?;

        let path = PathBuf::from(socket_path);
        self.state
            .node
            .lock()
            .await
            .set_topic_forward_socket(topic_bytes, path);
        Ok(true)
    }

    async fn remove_topic_forward_socket(&self, topic: String) -> RpcResult<bool> {
        debug!("Removing forward socket for topic via RPC");

        // Decode the base64 topic
        let topic_bytes = base64::engine::general_purpose::STANDARD
            .decode(topic.as_bytes())
            .map_err(|_| ErrorObject::from(ErrorCode::from(-32021)))?;

        self.state
            .node
            .lock()
            .await
            .delete_topic_forward_socket(topic_bytes);
        Ok(true)
    }
}

/// JSON-RPC API server handle. The server is spawned in a background task. If this handle is dropped,
/// the server is terminated.
pub struct JsonRpc {
    /// JSON-RPC server handle
    _server: ServerHandle,
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
    pub async fn spawn<M>(node: Arc<Mutex<mycelium::Node<M>>>, listen_addr: SocketAddr) -> Self
    where
        M: Metrics + Clone + Send + Sync + 'static,
    {
        debug!(%listen_addr, "Starting JSON-RPC server");

        let server_state = Arc::new(ServerState { node });

        // Create the server builder
        let server = ServerBuilder::default()
            .build(listen_addr)
            .await
            .expect("Failed to build JSON-RPC server");

        // Create the API implementation
        let api = RPCApi {
            state: server_state,
        };

        // Register the API implementation

        // Create the RPC module
        #[allow(unused_mut)]
        let mut methods = MyceliumApiServer::into_rpc(api.clone());

        // When the message feature is enabled, merge the message RPC module
        #[cfg(feature = "message")]
        {
            let message_methods = MyceliumMessageApiServer::into_rpc(api);
            methods
                .merge(message_methods)
                .expect("Can merge message API into base API");
        }

        // Start the server with the appropriate module(s)
        let handle = server.start(methods);

        debug!(%listen_addr, "JSON-RPC server started successfully");

        JsonRpc { _server: handle }
    }
}
