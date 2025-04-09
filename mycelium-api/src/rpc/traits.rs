//! RPC trait definitions for the Mycelium JSON-RPC API

use jsonrpc_core::Result as RpcResult;
use jsonrpc_derive::rpc;

use crate::Info;
use crate::Route;
use crate::QueriedSubnet;
use crate::NoRouteSubnet;
use mycelium::crypto::PublicKey;
use mycelium::peer_manager::PeerStats;
use mycelium::message::{MessageId, MessageInfo};

// Admin-related RPC methods
#[rpc]
pub trait AdminApi {
    /// Get general info about the node
    #[rpc(name = "getInfo")]
    fn get_info(&self) -> RpcResult<Info>;
    
    /// Get the pubkey from node ip
    #[rpc(name = "getPublicKeyFromIp")]
    fn get_pubkey_from_ip(&self, mycelium_ip: String) -> RpcResult<PublicKey>;
}

// Peer-related RPC methods
#[rpc]
pub trait PeerApi {
    /// List known peers
    #[rpc(name = "getPeers")]
    fn get_peers(&self) -> RpcResult<Vec<PeerStats>>;
    
    /// Add a new peer
    #[rpc(name = "addPeer")]
    fn add_peer(&self, endpoint: String) -> RpcResult<bool>;
    
    /// Remove an existing peer
    #[rpc(name = "deletePeer")]
    fn delete_peer(&self, endpoint: String) -> RpcResult<bool>;
}

// Route-related RPC methods
#[rpc]
pub trait RouteApi {
    /// List all selected routes
    #[rpc(name = "getSelectedRoutes")]
    fn get_selected_routes(&self) -> RpcResult<Vec<Route>>;
    
    /// List all active fallback routes
    #[rpc(name = "getFallbackRoutes")]
    fn get_fallback_routes(&self) -> RpcResult<Vec<Route>>;
    
    /// List all currently queried subnets
    #[rpc(name = "getQueriedSubnets")]
    fn get_queried_subnets(&self) -> RpcResult<Vec<QueriedSubnet>>;
    
    /// List all subnets which are explicitly marked as no route
    #[rpc(name = "getNoRouteEntries")]
    fn get_no_route_entries(&self) -> RpcResult<Vec<NoRouteSubnet>>;
}

// Message-related RPC methods
#[rpc]
pub trait MessageApi {
    /// Get a message from the inbound message queue
    #[rpc(name = "popMessage")]
    fn pop_message(&self, peek: Option<bool>, timeout: Option<u64>, topic: Option<String>) -> RpcResult<crate::message::MessageReceiveInfo>;
    
    /// Submit a new message to the system
    #[rpc(name = "pushMessage")]
    fn push_message(&self, message: crate::message::MessageSendInfo, reply_timeout: Option<u64>) -> RpcResult<crate::message::PushMessageResponse>;
    
    /// Reply to a message with the given ID
    #[rpc(name = "pushMessageReply")]
    fn push_message_reply(&self, id: String, message: crate::message::MessageSendInfo) -> RpcResult<bool>;
    
    /// Get the status of an outbound message
    #[rpc(name = "getMessageInfo")]
    fn get_message_info(&self, id: String) -> RpcResult<MessageInfo>;
}