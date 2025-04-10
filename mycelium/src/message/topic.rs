use crate::subnet::Subnet;
use core::fmt;
use serde::{
    de::{Deserialize, Deserializer, Visitor},
    Deserialize as DeserializeMacro,
};
use std::collections::HashMap;

#[derive(Debug, Default, Clone)]
pub struct TopicConfig {
    /// The default action to to take if no acl is defined for a topic.
    default: MessageAction,
    /// Explicitly configured whitelists for topics. Ip's which aren't part of the whitelist will
    /// not be allowed to send messages to that topic. If a topic is not in this map, the default
    /// action will be used.
    whitelist: HashMap<Vec<u8>, Vec<Subnet>>,
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
    pub fn whitelist(&self) -> &HashMap<Vec<u8>, Vec<Subnet>> {
        &self.whitelist
    }

    /// Insert a new topic in the whitelist, without any configured allowed sources.
    pub fn add_topic_whitelist(&mut self, topic: Vec<u8>) {
        self.whitelist.entry(topic).or_default();
    }

    /// Remove a topic from the whitelist. Future messages will follow the default action.
    pub fn remove_topic_whitelist(&mut self, topic: &Vec<u8>) {
        self.whitelist.remove(topic);
    }

    /// Adds a new whitelisted source for a topic. This creates the topic if it does not exist yet.
    pub fn add_topic_whitelist_src(&mut self, topic: Vec<u8>, src: Subnet) {
        self.whitelist.entry(topic).or_default().push(src)
    }

    /// Removes a whitelisted source for a topic.
    ///
    /// If the last source is removed for a topic, the entry remains, and must be cleared by calling
    /// [`Self::remove_topic_whitelist`] to fall back to the default action. Note that an empty
    /// whitelist effectively blocks all messages for a topic.
    ///
    /// This does nothing if the topic does not exist.
    pub fn remove_topic_whitelist_src(&mut self, topic: &Vec<u8>, src: Subnet) {
        if let Some(whitelist) = self.whitelist.get_mut(topic) {
            whitelist.retain(|e| e != &src);
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
                V: serde::de::MapAccess<'de>,
            {
                let mut default = MessageAction::default();
                let mut whitelist = HashMap::new();

                while let Some(key) = map.next_key::<String>()? {
                    if key == "default" {
                        default = map.next_value()?;
                    } else {
                        // Parse subnet list from string representations
                        let subnet_strs = map.next_value::<Vec<String>>()?;
                        let subnets = subnet_strs
                            .into_iter()
                            .map(|s| {
                                // Try to parse as a subnet (with prefix)
                                if let Ok(ipnet) = s.parse::<ipnet::IpNet>() {
                                    return Subnet::new(ipnet.addr(), ipnet.prefix_len()).map_err(
                                        |e| {
                                            serde::de::Error::custom(format!(
                                                "Invalid subnet prefix length: {}",
                                                e
                                            ))
                                        },
                                    );
                                }

                                // Try to parse as an IP address (convert to /128 subnet)
                                if let Ok(ip) = s.parse::<std::net::IpAddr>() {
                                    let prefix_len = match ip {
                                        std::net::IpAddr::V4(_) => 32,
                                        std::net::IpAddr::V6(_) => 128,
                                    };
                                    return Subnet::new(ip, prefix_len).map_err(|e| {
                                        serde::de::Error::custom(format!(
                                            "Invalid subnet prefix length: {}",
                                            e
                                        ))
                                    });
                                }

                                Err(serde::de::Error::custom(format!(
                                    "Invalid subnet or IP address: {}",
                                    s
                                )))
                            })
                            .collect::<Result<Vec<Subnet>, V::Error>>()?;

                        // Convert string key to Vec<u8>
                        whitelist.insert(key.into_bytes(), subnets);
                    }
                }

                Ok(TopicConfig { default, whitelist })
            }
        }

        deserializer.deserialize_map(TopicConfigVisitor)
    }
}
