use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct BridgeInfo {
    pub name: String,
    pub ifindex: u32,
    pub state: String,
    pub managed: bool,
    pub ports: Vec<String>,
    pub addresses: Vec<AddressInfo>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AddressInfo {
    pub interface: String,
    pub ifindex: u32,
    pub address: String,
    pub prefix_len: u8,
    pub scope: String,
    pub family: String,
    pub managed: bool,
    pub active_listener: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ListenerInfo {
    pub interface: String,
    pub address: String,
    pub port: u16,
    pub source: String,
    pub active: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct NetworkStatus {
    pub platform: String,
    pub listener_policy: String,
    pub supports_bridge_management: bool,
    pub supports_address_management: bool,
    pub bridges: u32,
    pub managed_bridges: u32,
    pub mycelium_addresses: u32,
    pub active_listeners: u32,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ListenerPolicy {
    AllMyceliumAddresses,
    AllManagedAddresses,
    ExplicitOnly,
}

impl Default for ListenerPolicy {
    fn default() -> Self {
        Self::AllMyceliumAddresses
    }
}

impl ListenerPolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::AllMyceliumAddresses => "all_mycelium_addresses",
            Self::AllManagedAddresses => "all_managed_addresses",
            Self::ExplicitOnly => "explicit_only",
        }
    }
}
