//! Minimal JSON-RPC 2.0 client over HTTP/1.1 on a Unix domain socket.

use std::path::Path;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::rt::TokioIo;
use serde_json::Value;
use tokio::net::UnixStream;

pub async fn rpc_call(
    socket_path: &Path,
    method: &str,
    params: Value,
) -> Result<Value, Box<dyn std::error::Error>> {
    let stream = UnixStream::connect(socket_path).await
        .map_err(|e| format!("Cannot connect to {}: {e}", socket_path.display()))?;

    let io = TokioIo::new(stream);

    let (mut sender, conn) = hyper::client::conn::http1::Builder::new()
        .handshake::<_, Full<Bytes>>(io)
        .await
        .map_err(|e| format!("HTTP handshake failed: {e}"))?;

    tokio::spawn(conn);

    let payload = serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
        "id": 1
    });
    let body_bytes = serde_json::to_vec(&payload)?;

    let req = Request::builder()
        .method("POST")
        .uri("/rpc")
        .header("host", "localhost")
        .header("content-type", "application/json")
        .header("content-length", body_bytes.len())
        .body(Full::new(Bytes::from(body_bytes)))
        .map_err(|e| format!("Failed to build request: {e}"))?;

    let resp = sender
        .send_request(req)
        .await
        .map_err(|e| format!("Failed to send request: {e}"))?;

    let status = resp.status();
    let body = resp
        .into_body()
        .collect()
        .await
        .map_err(|e| format!("Failed to collect response body: {e}"))?
        .to_bytes();

    if status.as_u16() == 204 {
        return Ok(Value::Null);
    }

    if !status.is_success() {
        return Err(
            format!("HTTP error {}: {}", status, String::from_utf8_lossy(&body)).into(),
        );
    }

    let rpc_resp: Value = serde_json::from_slice(&body)?;

    if let Some(error) = rpc_resp.get("error") {
        let code = error.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
        let msg = error
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown");
        return Err(format!("RPC error {code}: {msg}").into());
    }

    Ok(rpc_resp.get("result").cloned().unwrap_or(Value::Null))
}
