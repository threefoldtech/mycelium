//! Admin-related JSON-RPC methods for the Mycelium API

use jsonrpc_core::{Error, ErrorCode, Result as RpcResult};
use std::net::IpAddr;
use std::str::FromStr;
use tracing::debug;

use mycelium::crypto::PublicKey;
use mycelium::metrics::Metrics;

use crate::HttpServerState;
use crate::Info;
use crate::rpc::models::error_codes;
use crate::rpc::traits::AdminApi;

/// Implementation of Admin-related JSON-RPC methods
pub struct AdminRpc<M>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    state: HttpServerState<M>,
}

impl<M> AdminRpc<M>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    /// Create a new AdminRpc instance
    pub fn new(state: HttpServerState<M>) -> Self {
        Self { state }
    }
}

impl<M> AdminApi for AdminRpc<M>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    fn get_info(&self) -> RpcResult<Info> {
        debug!("Getting node info via RPC");
        let info = self.state.node.blocking_lock().info();
        Ok(Info {
            node_subnet: info.node_subnet.to_string(),
            node_pubkey: info.node_pubkey,
        })
    }
    
    fn get_pubkey_from_ip(&self, mycelium_ip: String) -> RpcResult<PublicKey> {
        debug!(ip = %mycelium_ip, "Getting public key from IP via RPC");
        let ip = IpAddr::from_str(&mycelium_ip).map_err(|e| Error {
            code: ErrorCode::InvalidParams,
            message: format!("Invalid IP address: {}", e),
            data: None,
        })?;
        
        match self.state.node.blocking_lock().get_pubkey_from_ip(ip) {
            Some(pubkey) => Ok(pubkey),
            None => Err(Error {
                code: ErrorCode::ServerError(error_codes::PUBKEY_NOT_FOUND),
                message: "Public key not found".to_string(),
                data: None,
            }),
        }
    }
}