use std::{
    io::Write,
    mem,
    net::{IpAddr, SocketAddr},
    path::PathBuf,
};

use base64::{
    alphabet,
    engine::{GeneralPurpose, GeneralPurposeConfig},
    Engine,
};
use mycelium::{crypto::PublicKey, message::MessageId, subnet::Subnet};
use serde::{Serialize, Serializer};
use tracing::{debug, error};

use mycelium_api::{MessageDestination, MessageReceiveInfo, MessageSendInfo, PushMessageResponse};

enum Payload {
    Readable(String),
    NotReadable(Vec<u8>),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CliMessage {
    id: MessageId,
    src_ip: IpAddr,
    src_pk: PublicKey,
    dst_ip: IpAddr,
    dst_pk: PublicKey,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(serialize_with = "serialize_payload")]
    topic: Option<Payload>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(serialize_with = "serialize_payload")]
    payload: Option<Payload>,
}

const B64ENGINE: GeneralPurpose = base64::engine::general_purpose::GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new(),
);
fn serialize_payload<S: Serializer>(p: &Option<Payload>, s: S) -> Result<S::Ok, S::Error> {
    let base64 = match p {
        None => None,
        Some(Payload::Readable(data)) => Some(data.clone()),
        Some(Payload::NotReadable(data)) => Some(B64ENGINE.encode(data)),
    };
    <Option<String>>::serialize(&base64, s)
}

/// Encode arbitrary data in standard base64.
pub fn encode_base64(input: &[u8]) -> String {
    B64ENGINE.encode(input)
}

/// Send a message to a receiver.
#[allow(clippy::too_many_arguments)]
pub async fn send_msg(
    destination: String,
    msg: Option<String>,
    wait: bool,
    timeout: Option<u64>,
    reply_to: Option<String>,
    topic: Option<String>,
    msg_path: Option<PathBuf>,
    server_addr: SocketAddr,
) -> Result<(), Box<dyn std::error::Error>> {
    if reply_to.is_some() && wait {
        error!("Can't wait on a reply for a reply, either use --reply-to or --wait");
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Only one of --reply-to or --wait is allowed",
        )
        .into());
    }
    let destination = if destination.len() == 64 {
        // Public key in hex format
        match PublicKey::try_from(&*destination) {
            Err(_) => {
                error!("{destination} is not a valid hex encoded public key");
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "Invalid hex encoded public key",
                )
                .into());
            }
            Ok(pk) => MessageDestination::Pk(pk),
        }
    } else {
        match destination.parse() {
            Err(e) => {
                error!("{destination} is not a valid IPv6 address: {e}");
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "Invalid IPv6 address",
                )
                .into());
            }
            Ok(ip) => {
                let global_subnet = Subnet::new(
                    mycelium::GLOBAL_SUBNET_ADDRESS,
                    mycelium::GLOBAL_SUBNET_PREFIX_LEN,
                )
                .unwrap();
                if !global_subnet.contains_ip(ip) {
                    error!("{destination} is not a part of {global_subnet}");
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "IPv6 address is not part of the mycelium subnet",
                    )
                    .into());
                }
                MessageDestination::Ip(ip)
            }
        }
    };

    // Load msg, files have prio.
    let msg = if let Some(path) = msg_path {
        match tokio::fs::read(&path).await {
            Err(e) => {
                error!("Could not read file at {:?}: {e}", path);
                return Err(e.into());
            }
            Ok(data) => data,
        }
    } else if let Some(msg) = msg {
        msg.into_bytes()
    } else {
        error!("Message is a required argument if `--msg-path` is not provided");
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Message is a required argument if `--msg-path` is not provided",
        )
        .into());
    };

    let mut url = format!("http://{server_addr}/api/v1/messages");
    if let Some(reply_to) = reply_to {
        url.push_str(&format!("/reply/{reply_to}"));
    }
    if wait {
        // A year should be sufficient to wait
        let reply_timeout = timeout.unwrap_or(60 * 60 * 24 * 365);
        url.push_str(&format!("?reply_timeout={reply_timeout}"));
    }

    match reqwest::Client::new()
        .post(url)
        .json(&MessageSendInfo {
            dst: destination,
            topic: topic.map(String::into_bytes),
            payload: msg,
        })
        .send()
        .await
    {
        Err(e) => {
            error!("Failed to send request: {e}");
            return Err(e.into());
        }
        Ok(res) => {
            if res.status() == STATUSCODE_NO_CONTENT {
                return Ok(());
            }
            match res.json::<PushMessageResponse>().await {
                Err(e) => {
                    error!("Failed to load response body {e}");
                    return Err(e.into());
                }
                Ok(resp) => {
                    match resp {
                        PushMessageResponse::Id(id) => {
                            let _ = serde_json::to_writer(std::io::stdout(), &id);
                        }
                        PushMessageResponse::Reply(mri) => {
                            let cm = CliMessage {
                                id: mri.id,

                                topic: mri.topic.map(|topic| {
                                    if let Ok(s) = String::from_utf8(topic.clone()) {
                                        Payload::Readable(s)
                                    } else {
                                        Payload::NotReadable(topic)
                                    }
                                }),
                                src_ip: mri.src_ip,
                                src_pk: mri.src_pk,
                                dst_ip: mri.dst_ip,
                                dst_pk: mri.dst_pk,
                                payload: Some({
                                    if let Ok(s) = String::from_utf8(mri.payload.clone()) {
                                        Payload::Readable(s)
                                    } else {
                                        Payload::NotReadable(mri.payload)
                                    }
                                }),
                            };
                            let _ = serde_json::to_writer(std::io::stdout(), &cm);
                        }
                    }
                    println!();
                }
            }
        }
    }

    Ok(())
}

const STATUSCODE_NO_CONTENT: u16 = 204;

pub async fn recv_msg(
    timeout: Option<u64>,
    topic: Option<String>,
    msg_path: Option<PathBuf>,
    raw: bool,
    server_addr: SocketAddr,
) -> Result<(), Box<dyn std::error::Error>> {
    // One year timeout should be sufficient
    let timeout = timeout.unwrap_or(60 * 60 * 24 * 365);
    let mut url = format!("http://{server_addr}/api/v1/messages?timeout={timeout}");
    if let Some(ref topic) = topic {
        if topic.len() > 255 {
            error!("{topic} is longer than the maximum allowed topic length of 255");
            return Err(
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "Topic too long").into(),
            );
        }
        url.push_str(&format!("&topic={}", encode_base64(topic.as_bytes())));
    }
    let mut cm = match reqwest::get(url).await {
        Err(e) => {
            error!("Failed to wait for message: {e}");
            return Err(e.into());
        }
        Ok(resp) => {
            if resp.status() == STATUSCODE_NO_CONTENT {
                debug!("No message ready yet");
                return Ok(());
            }

            debug!("Received message response");
            match resp.json::<MessageReceiveInfo>().await {
                Err(e) => {
                    error!("Failed to load response json: {e}");
                    return Err(e.into());
                }
                Ok(mri) => CliMessage {
                    id: mri.id,
                    topic: mri.topic.map(|topic| {
                        if let Ok(s) = String::from_utf8(topic.clone()) {
                            Payload::Readable(s)
                        } else {
                            Payload::NotReadable(topic)
                        }
                    }),
                    src_ip: mri.src_ip,
                    src_pk: mri.src_pk,
                    dst_ip: mri.dst_ip,
                    dst_pk: mri.dst_pk,
                    payload: Some({
                        if let Ok(s) = String::from_utf8(mri.payload.clone()) {
                            Payload::Readable(s)
                        } else {
                            Payload::NotReadable(mri.payload)
                        }
                    }),
                },
            }
        }
    };

    if let Some(ref file_path) = msg_path {
        if let Err(e) = tokio::fs::write(
            &file_path,
            match mem::take(&mut cm.payload).unwrap() {
                Payload::Readable(ref s) => s as &dyn AsRef<[u8]>,
                Payload::NotReadable(ref v) => v,
            },
        )
        .await
        {
            error!("Failed to write response payload to file: {e}");
            return Err(e.into());
        }
    }

    if raw {
        // only print payload if not already written
        if msg_path.is_none() {
            let _ = std::io::stdout().write_all(match cm.payload.unwrap() {
                Payload::Readable(ref s) => s.as_bytes(),
                Payload::NotReadable(ref v) => v,
            });
            println!();
        }
    } else {
        let _ = serde_json::to_writer(std::io::stdout(), &cm);
        println!();
    }

    Ok(())
}
