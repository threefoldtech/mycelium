//! Models for the Mycelium JSON-RPC API

use serde::{Deserialize, Serialize};

// Define any additional models needed for the JSON-RPC API
// Most models can be reused from the existing REST API

/// Error codes for the JSON-RPC API
pub mod error_codes {
    /// Invalid parameters error code
    pub const INVALID_PARAMS: i64 = -32602;
    
    /// Peer already exists error code
    pub const PEER_EXISTS: i64 = 409;
    
    /// Peer not found error code
    pub const PEER_NOT_FOUND: i64 = 404;
    
    /// Message not found error code
    pub const MESSAGE_NOT_FOUND: i64 = 404;
    
    /// Public key not found error code
    pub const PUBKEY_NOT_FOUND: i64 = 404;
    
    /// No message ready error code
    pub const NO_MESSAGE_READY: i64 = 204;
    
    /// Timeout waiting for reply error code
    pub const TIMEOUT_WAITING_FOR_REPLY: i64 = 408;
}