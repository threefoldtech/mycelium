use crate::subnet::Subnet;
use core::fmt;
use serde::{
    de::{Deserialize, Deserializer, MapAccess, Visitor},
    Deserialize as DeserializeMacro,
};
use std::collections::HashMap;
use std::path::PathBuf;

/// Configuration for a topic whitelist, including allowed subnets and optional forward socket
#[derive(Debug, Default, Clone)]
pub struct TopicWhitelistConfig {
    /// Subnets that are allowed to send messages to this topic
    subnets: Vec<Subnet>,
    /// Optional Unix domain socket path to forward messages to
    forward_socket: Option<PathBuf>,
}

impl TopicWhitelistConfig {
    /// Create a new empty whitelist config
    pub fn new() -> Self {
        Self::default()
    }

    /// Get the list of whitelisted subnets
    pub fn subnets(&self) -> &Vec<Subnet> {
        &self.subnets
    }

    /// Get the forward socket path, if any
    pub fn forward_socket(&self) -> Option<&PathBuf> {
        self.forward_socket.as_ref()
    }

    /// Set the forward socket path
    pub fn set_forward_socket(&mut self, path: Option<PathBuf>) {
        self.forward_socket = path;
    }

    /// Add a subnet to the whitelist
    pub fn add_subnet(&mut self, subnet: Subnet) {
        self.subnets.push(subnet);
    }

    /// Remove a subnet from the whitelist
    pub fn remove_subnet(&mut self, subnet: &Subnet) {
        self.subnets.retain(|s| s != subnet);
    }
}

#[derive(Debug, Default, Clone)]
pub struct TopicConfig {
    /// The default action to to take if no acl is defined for a topic.
    default: MessageAction,
    /// Explicitly configured whitelists for topics. Ip's which aren't part of the whitelist will
    /// not be allowed to send messages to that topic. If a topic is not in this map, the default
    /// action will be used.
    whitelist: HashMap<Vec<u8>, TopicWhitelistConfig>,
}

impl TopicConfig {
    /// Get the [`default action`](MessageAction) if the topic is not configured.
    pub fn default(&self) -> MessageAction {
        self.default
    }

    /// Set the default [`action`](MessageAction) which does not have a whitelist configured.
    pub fn set_default(&mut self, default: MessageAction) {
        self.default = default;
    }

    /// Get the fully configured whitelist
    pub fn whitelist(&self) -> &HashMap<Vec<u8>, TopicWhitelistConfig> {
        &self.whitelist
    }

    /// Insert a new topic in the whitelist, without any configured allowed sources.
    pub fn add_topic_whitelist(&mut self, topic: Vec<u8>) {
        self.whitelist.entry(topic).or_default();
    }

    /// Set the forward socket for a topic. Does nothing if the topic doesn't exist.
    pub fn set_topic_forward_socket(&mut self, topic: Vec<u8>, socket_path: Option<PathBuf>) {
        self.whitelist
            .entry(topic)
            .and_modify(|c| c.set_forward_socket(socket_path));
    }

    /// Get the forward socket for a topic, if any.
    pub fn get_topic_forward_socket(&self, topic: &Vec<u8>) -> Option<&PathBuf> {
        self.whitelist
            .get(topic)
            .and_then(|config| config.forward_socket())
    }

    /// Remove a topic from the whitelist. Future messages will follow the default action.
    pub fn remove_topic_whitelist(&mut self, topic: &Vec<u8>) {
        self.whitelist.remove(topic);
    }

    /// Adds a new whitelisted source for a topic. This creates the topic if it does not exist yet.
    pub fn add_topic_whitelist_src(&mut self, topic: Vec<u8>, src: Subnet) {
        self.whitelist.entry(topic).or_default().add_subnet(src);
    }

    /// Removes a whitelisted source for a topic.
    ///
    /// If the last source is removed for a topic, the entry remains, and must be cleared by calling
    /// [`Self::remove_topic_whitelist`] to fall back to the default action. Note that an empty
    /// whitelist effectively blocks all messages for a topic.
    ///
    /// This does nothing if the topic does not exist.
    pub fn remove_topic_whitelist_src(&mut self, topic: &Vec<u8>, src: Subnet) {
        if let Some(whitelist_config) = self.whitelist.get_mut(topic) {
            whitelist_config.remove_subnet(&src);
        }
    }
}

#[derive(Debug, Default, Clone, Copy, DeserializeMacro)]
pub enum MessageAction {
    /// Accept the message
    #[default]
    Accept,
    /// Reject the message
    Reject,
}

// Helper function to parse a subnet from a string
fn parse_subnet_str<E>(s: &str) -> Result<Subnet, E>
where
    E: serde::de::Error,
{
    // Try to parse as a subnet (with prefix)
    if let Ok(ipnet) = s.parse::<ipnet::IpNet>() {
        return Subnet::new(ipnet.addr(), ipnet.prefix_len())
            .map_err(|e| serde::de::Error::custom(format!("Invalid subnet prefix length: {}", e)));
    }

    // Try to parse as an IP address (convert to /32 or /128 subnet)
    if let Ok(ip) = s.parse::<std::net::IpAddr>() {
        let prefix_len = match ip {
            std::net::IpAddr::V4(_) => 32,
            std::net::IpAddr::V6(_) => 128,
        };
        return Subnet::new(ip, prefix_len)
            .map_err(|e| serde::de::Error::custom(format!("Invalid subnet prefix length: {}", e)));
    }

    Err(serde::de::Error::custom(format!(
        "Invalid subnet or IP address: {}",
        s
    )))
}

// Define a struct for deserializing the whitelist config
#[derive(DeserializeMacro)]
struct WhitelistConfigData {
    #[serde(default)]
    subnets: Vec<String>,
    #[serde(default)]
    forward_socket: Option<String>,
}

// Add this implementation right after the TopicConfig struct definition
impl<'de> Deserialize<'de> for TopicConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct TopicConfigVisitor;

        impl<'de> Visitor<'de> for TopicConfigVisitor {
            type Value = TopicConfig;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a topic configuration")
            }

            fn visit_map<V>(self, mut map: V) -> Result<TopicConfig, V::Error>
            where
                V: MapAccess<'de>,
            {
                let mut default = MessageAction::default();
                let mut whitelist = HashMap::new();

                while let Some(key) = map.next_key::<String>()? {
                    if key == "default" {
                        default = map.next_value()?;
                    } else {
                        // Try to parse as a WhitelistConfigData first
                        if let Ok(config_data) = map.next_value::<WhitelistConfigData>() {
                            let mut whitelist_config = TopicWhitelistConfig::default();

                            // Process subnets
                            for subnet_str in config_data.subnets {
                                let subnet = parse_subnet_str(&subnet_str)?;
                                whitelist_config.add_subnet(subnet);
                            }

                            // Process forward_socket
                            if let Some(socket_path) = config_data.forward_socket {
                                whitelist_config
                                    .set_forward_socket(Some(PathBuf::from(socket_path)));
                            }

                            // Convert string key to Vec<u8>
                            whitelist.insert(key.into_bytes(), whitelist_config);
                        } else {
                            // Fallback to old format: just a list of subnets
                            let subnet_strs = map.next_value::<Vec<String>>()?;
                            let mut whitelist_config = TopicWhitelistConfig::default();

                            for subnet_str in subnet_strs {
                                let subnet = parse_subnet_str(&subnet_str)?;
                                whitelist_config.add_subnet(subnet);
                            }

                            // Convert string key to Vec<u8>
                            whitelist.insert(key.into_bytes(), whitelist_config);
                        }
                    }
                }

                Ok(TopicConfig { default, whitelist })
            }
        }

        deserializer.deserialize_map(TopicConfigVisitor)
    }
}
