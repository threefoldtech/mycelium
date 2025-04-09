# Mycelium Unix Socket OpenRPC API

This document describes the Unix socket-based OpenRPC API for Mycelium.

## Overview

The Unix socket OpenRPC API provides a way to interact with the Mycelium daemon through a Unix socket. This is particularly useful for local applications that need to communicate with Mycelium without using the HTTP API.

The API follows the [JSON-RPC 2.0](https://www.jsonrpc.org/specification) specification.

## Connection Model

The Unix socket API uses a "one message per connection" model:

1. Client connects to the Unix socket
2. Client sends a single JSON-RPC request
3. Server processes the request and sends a response
4. Connection is closed

This model is simple and efficient for local communication, as it avoids the overhead of maintaining persistent connections.

## Socket Location

By default, the Unix socket is created at `/tmp/mycelium.sock`. This can be configured when starting the Mycelium daemon using the `--unix-socket-path` option.

## Available Methods

The Unix socket API provides the same methods as the HTTP API:

- `getInfo`: Get information about the node
- `getPeers`: Get a list of connected peers
- `addPeer`: Add a new peer
- `deletePeer`: Remove a peer
- `getSelectedRoutes`: Get selected routes
- `getFallbackRoutes`: Get fallback routes
- `getPubkeyFromIp`: Get public key from IP
- `getMessage`: Get a message
- `pushMessage`: Push a new message
- `replyMessage`: Reply to a message
- `messageStatus`: Get message status

## Example Usage

See the `mycelium-api-unix-socket-example.py` script for examples of how to use the Unix socket API.

Basic example in Python:

```python
import socket
import json

# Create a Unix socket
sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)

# Connect to the socket
sock.connect("/tmp/mycelium-api.sock")

# Create a JSON-RPC request
request = {
    "jsonrpc": "2.0",
    "id": 1,
    "method": "getInfo"
}

# Send the request
sock.sendall(json.dumps(request).encode('utf-8'))

# Receive the response
response = b""
while True:
    data = sock.recv(4096)
    if not data:
        break
    response += data

# Parse the response
result = json.loads(response.decode('utf-8'))
print(result)

# Close the socket
sock.close()
```

## Error Handling

The API follows the JSON-RPC 2.0 error handling specification. Errors are returned as JSON objects with the following structure:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "error": {
    "code": -32700,
    "message": "Parse error",
    "data": "Error details"
  }
}
```

Common error codes:
- `-32700`: Parse error
- `-32600`: Invalid Request
- `-32601`: Method not found
- `-32602`: Invalid params
- `400`: Bad request
- `404`: Not found
- `409`: Conflict