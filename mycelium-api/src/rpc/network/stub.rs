use std::net::Ipv6Addr;
use std::sync::{Arc, Mutex};

use super::errors::NetworkError;
use super::managed::ManagedState;
use super::models::{
    AddressInfo, BridgeInfo, ListenerInfo, ListenerPolicy, NetworkStatus,
};

type Shared = Arc<Mutex<ManagedState>>;

pub async fn list_bridges(
    _managed: Shared,
    _include_addresses: bool,
    _include_ports: bool,
    _managed_only: bool,
) -> Result<Vec<BridgeInfo>, NetworkError> {
    Err(NetworkError::UnsupportedPlatform)
}

pub async fn ensure_bridge(
    _managed: Shared,
    _name: String,
    _up: bool,
    _mtu: Option<u32>,
) -> Result<(BridgeInfo, bool), NetworkError> {
    Err(NetworkError::UnsupportedPlatform)
}

pub async fn delete_bridge(
    _managed: Shared,
    _name: String,
    _only_if_empty: bool,
) -> Result<bool, NetworkError> {
    Err(NetworkError::UnsupportedPlatform)
}

pub async fn list_addresses(
    _managed: Shared,
    _interface: Option<String>,
    _bridge_only: bool,
    _mycelium_only: bool,
    _managed_only: bool,
) -> Result<Vec<AddressInfo>, NetworkError> {
    Err(NetworkError::UnsupportedPlatform)
}

pub async fn add_address(
    _managed: Shared,
    _interface: String,
    _address: Ipv6Addr,
    _prefix_len: u8,
    _activate_listener: bool,
) -> Result<(String, String, u8, bool, bool), NetworkError> {
    Err(NetworkError::UnsupportedPlatform)
}

pub async fn remove_address(
    _managed: Shared,
    _interface: String,
    _address: String,
) -> Result<(bool, bool), NetworkError> {
    Err(NetworkError::UnsupportedPlatform)
}

pub async fn get_listeners(
    _managed: Shared,
) -> Result<(String, Vec<ListenerInfo>), NetworkError> {
    Err(NetworkError::UnsupportedPlatform)
}

pub async fn set_listener_policy(
    managed: Shared,
    policy: ListenerPolicy,
    explicit: Option<Vec<String>>,
) -> Result<String, NetworkError> {
    let mut guard = managed.lock().map_err(|e| NetworkError::Internal(e.to_string()))?;
    guard.policy = policy;
    if let Some(e) = explicit {
        guard.explicit_addresses = e;
    }
    Ok(guard.policy.as_str().to_string())
}

pub fn parse_address_with_prefix(
    s: &str,
    default: Option<u8>,
) -> Result<(Ipv6Addr, Option<u8>), NetworkError> {
    if let Some((addr, p)) = s.split_once('/') {
        let ip: Ipv6Addr = addr
            .parse()
            .map_err(|_| NetworkError::InvalidArgument(format!("invalid IPv6 address: {addr}")))?;
        let prefix: u8 = p
            .parse()
            .map_err(|_| NetworkError::InvalidArgument(format!("invalid prefix: {p}")))?;
        if prefix > 128 {
            return Err(NetworkError::InvalidArgument("prefix > 128".into()));
        }
        Ok((ip, Some(prefix)))
    } else {
        let ip: Ipv6Addr = s
            .parse()
            .map_err(|_| NetworkError::InvalidArgument(format!("invalid IPv6 address: {s}")))?;
        Ok((ip, default))
    }
}

pub async fn get_status(managed: Shared) -> Result<NetworkStatus, NetworkError> {
    let policy = {
        let guard = managed.lock().map_err(|e| NetworkError::Internal(e.to_string()))?;
        guard.policy.as_str().to_string()
    };
    Ok(NetworkStatus {
        platform: std::env::consts::OS.to_string(),
        listener_policy: policy,
        supports_bridge_management: false,
        supports_address_management: false,
        bridges: 0,
        managed_bridges: 0,
        mycelium_addresses: 0,
        active_listeners: 0,
    })
}
