//! Peer-related JSON-RPC methods for the Mycelium API

use jsonrpc_core::{Error, ErrorCode, Result as RpcResult};
use std::str::FromStr;
use tracing::debug;

use mycelium::endpoint::Endpoint;
use mycelium::metrics::Metrics;
use mycelium::peer_manager::{PeerExists, PeerNotFound, PeerStats};

use crate::rpc::models::error_codes;
use crate::rpc::traits::PeerApi;
use crate::HttpServerState;

/// Implementation of Peer-related JSON-RPC methods
pub struct PeerRpc<M>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    state: HttpServerState<M>,
}

impl<M> PeerRpc<M>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    /// Create a new PeerRpc instance
    pub fn new(state: HttpServerState<M>) -> Self {
        Self { state }
    }
}

impl<M> PeerApi for PeerRpc<M>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    fn get_peers(&self) -> RpcResult<Vec<PeerStats>> {
        debug!("Fetching peer stats via RPC");
        Ok(self.state.node.blocking_lock().peer_info())
    }

    fn add_peer(&self, endpoint: String) -> RpcResult<bool> {
        debug!(
            peer.endpoint = endpoint,
            "Attempting to add peer to the system via RPC"
        );

        let endpoint = Endpoint::from_str(&endpoint).map_err(|e| Error {
            code: ErrorCode::InvalidParams,
            message: e.to_string(),
            data: None,
        })?;

        match self.state.node.blocking_lock().add_peer(endpoint) {
            Ok(()) => Ok(true),
            Err(PeerExists) => Err(Error {
                code: ErrorCode::ServerError(error_codes::PEER_EXISTS),
                message: "A peer identified by that endpoint already exists".to_string(),
                data: None,
            }),
        }
    }

    fn delete_peer(&self, endpoint: String) -> RpcResult<bool> {
        debug!(
            peer.endpoint = endpoint,
            "Attempting to remove peer from the system via RPC"
        );

        let endpoint = Endpoint::from_str(&endpoint).map_err(|e| Error {
            code: ErrorCode::InvalidParams,
            message: e.to_string(),
            data: None,
        })?;

        match self.state.node.blocking_lock().remove_peer(endpoint) {
            Ok(()) => Ok(true),
            Err(PeerNotFound) => Err(Error {
                code: ErrorCode::ServerError(error_codes::PEER_NOT_FOUND),
                message: "A peer identified by that endpoint does not exist".to_string(),
                data: None,
            }),
        }
    }
}

