//! Message-related JSON-RPC methods for the Mycelium API

use jsonrpc_core::{Error, ErrorCode, Result as RpcResult};
use std::time::Duration;
use tracing::debug;

use mycelium::metrics::Metrics;
use mycelium::message::{MessageId, MessageInfo};

use crate::HttpServerState;
use crate::message::{MessageReceiveInfo, MessageSendInfo, PushMessageResponse};
use crate::rpc::models::error_codes;
use crate::rpc::traits::MessageApi;

/// Implementation of Message-related JSON-RPC methods
pub struct MessageRpc<M>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    state: HttpServerState<M>,
}

impl<M> MessageRpc<M>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    /// Create a new MessageRpc instance
    pub fn new(state: HttpServerState<M>) -> Self {
        Self { state }
    }
    
    /// Convert a base64 string to bytes
    fn decode_base64(&self, s: &str) -> Result<Vec<u8>, Error> {
        base64::engine::general_purpose::STANDARD.decode(s.as_bytes())
            .map_err(|e| Error {
                code: ErrorCode::InvalidParams,
                message: format!("Invalid base64 encoding: {}", e),
                data: None,
            })
    }
}

impl<M> MessageApi for MessageRpc<M>
where
    M: Metrics + Clone + Send + Sync + 'static,
{
    fn pop_message(&self, peek: Option<bool>, timeout: Option<u64>, topic: Option<String>) -> RpcResult<MessageReceiveInfo> {
        debug!(
            "Attempt to get message via RPC, peek {}, timeout {} seconds",
            peek.unwrap_or(false),
            timeout.unwrap_or(0)
        );
        
        let topic_bytes = if let Some(topic_str) = topic {
            Some(self.decode_base64(&topic_str)?)
        } else {
            None
        };
        
        // A timeout of 0 seconds essentially means get a message if there is one, and return
        // immediately if there isn't.
        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                tokio::time::timeout(
                    Duration::from_secs(timeout.unwrap_or(0)),
                    self.state
                        .node
                        .lock()
                        .await
                        .get_message(!peek.unwrap_or(false), topic_bytes),
                )
                .await
            })
        });
        
        match result {
            Ok(Ok(m)) => Ok(MessageReceiveInfo {
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
            }),
            _ => Err(Error {
                code: ErrorCode::ServerError(error_codes::NO_MESSAGE_READY),
                message: "No message ready".to_string(),
                data: None,
            }),
        }
    }
    
    fn push_message(&self, message: MessageSendInfo, reply_timeout: Option<u64>) -> RpcResult<PushMessageResponse> {
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
        
        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                self.state.node.lock().await.push_message(
                    dst,
                    message.payload,
                    message.topic,
                    DEFAULT_MESSAGE_TRY_DURATION,
                    reply_timeout.is_some(),
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
        
        if reply_timeout.is_none() {
            // If we don't wait for the reply just return here.
            return Ok(PushMessageResponse::Id(crate::message::MessageIdReply { id }));
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
                                    Ok(PushMessageResponse::Reply(MessageReceiveInfo {
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
                    _ = tokio::time::sleep(Duration::from_secs(reply_timeout.unwrap_or(0))) => {
                        // Timeout expired while waiting for reply
                        Ok(PushMessageResponse::Id(crate::message::MessageIdReply { id }))
                    }
                }
            })
        });
        
        match reply_result {
            Ok(response) => Ok(response),
            Err(e) => Err(e),
        }
    }
    
    fn push_message_reply(&self, id: String, message: MessageSendInfo) -> RpcResult<bool> {
        let message_id = match MessageId::from_hex(&id) {
            Ok(id) => id,
            Err(_) => {
                return Err(Error {
                    code: ErrorCode::InvalidParams,
                    message: "Invalid message ID".to_string(),
                    data: None,
                });
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
        
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                self.state.node.lock().await.reply_message(
                    message_id,
                    dst,
                    message.payload,
                    DEFAULT_MESSAGE_TRY_DURATION,
                );
            })
        });
        
        Ok(true)
    }
    
    fn get_message_info(&self, id: String) -> RpcResult<MessageInfo> {
        let message_id = match MessageId::from_hex(&id) {
            Ok(id) => id,
            Err(_) => {
                return Err(Error {
                    code: ErrorCode::InvalidParams,
                    message: "Invalid message ID".to_string(),
                    data: None,
                });
            }
        };
        
        debug!(message.id=%id, "Fetching message status via RPC");
        
        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                self.state.node.lock().await.message_status(message_id)
            })
        });
        
        match result {
            Some(info) => Ok(info),
            None => Err(Error {
                code: ErrorCode::ServerError(error_codes::MESSAGE_NOT_FOUND),
                message: "Message not found".to_string(),
                data: None,
            }),
        }
    }
}