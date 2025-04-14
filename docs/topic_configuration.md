# Topic Configuration Guide

This document explains how to configure message topics in Mycelium, including how to add new topics, configure socket forwarding paths, and manage whitelisted subnets.

## Overview

Mycelium's messaging system uses topics to categorize and route messages. Each topic can be configured with:

- **Whitelisted subnets**: IP subnets that are allowed to send messages to this topic
- **Forward socket**: A Unix domain socket path where messages for this topic will be forwarded

When a message is received with a topic that has a configured socket path, the content of the message is pushed to the socket, and the system waits for a reply from the socket, which is then sent back to the original sender.

## Configuration Using JSON-RPC API

The JSON-RPC API provides a comprehensive set of methods for managing topics, socket forwarding paths, and whitelisted subnets.

## Adding a New Topic

### Using the JSON-RPC API

```json
{
  "jsonrpc": "2.0",
  "method": "addTopic",
  "params": ["dGVzdC10b3BpYw=="],  // base64 encoding of "test-topic"
  "id": 1
}
```

Example using curl:

```bash
curl -X POST http://localhost:8990/rpc \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "addTopic",
    "params": ["dGVzdC10b3BpYw=="],
    "id": 1
  }'
```


## Configuring a Socket Forwarding Path

When a topic is configured with a socket forwarding path, messages for that topic will be forwarded to the specified Unix domain socket instead of being pushed to the message queue.

### Using the JSON-RPC API

```json
{
  "jsonrpc": "2.0",
  "method": "setTopicForwardSocket",
  "params": ["dGVzdC10b3BpYw==", "/path/to/socket"],
  "id": 1
}
```

Example using curl:

```bash
curl -X POST http://localhost:8990/rpc \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "setTopicForwardSocket",
    "params": ["dGVzdC10b3BpYw==", "/path/to/socket"],
    "id": 1
  }'
```
```

## Adding a Whitelisted Subnet

Whitelisted subnets control which IP addresses are allowed to send messages to a specific topic. If a message is received from an IP that is not in the whitelist, it will be dropped.

### Using the JSON-RPC API

```json
{
  "jsonrpc": "2.0",
  "method": "addTopicSource",
  "params": ["dGVzdC10b3BpYw==", "192.168.1.0/24"],
  "id": 1
}
```

Example using curl:

```bash
curl -X POST http://localhost:8990/rpc \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "addTopicSource",
    "params": ["dGVzdC10b3BpYw==", "192.168.1.0/24"],
    "id": 1
  }'
```
```

## Setting the Default Topic Action

You can configure the default action to take for topics that don't have explicit whitelist configurations:

### Using the JSON-RPC API

```json
{
  "jsonrpc": "2.0",
  "method": "setDefaultTopicAction",
  "params": [true],
  "id": 1
}
```

Example using curl:

```bash
curl -X POST http://localhost:8990/rpc \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "setDefaultTopicAction",
    "params": [true],
    "id": 1
  }'
```
```

## Socket Protocol

When a message is forwarded to a socket, the raw message data is sent to the socket. The socket is expected to process the message and send a reply, which will be forwarded back to the original sender.

The socket protocol is simple:
1. The message data is written to the socket
2. The system waits for a reply from the socket (with a configurable timeout)
3. The reply data is read from the socket and sent back to the original sender

## Example: Creating a Socket Server

Here's an example of a simple socket server that echoes back the received data:

```rust
use std::{
    io::{Read, Write},
    os::unix::net::UnixListener,
    path::Path,
    thread,
};

fn main() {
    let socket_path = "/tmp/mycelium-socket";
    
    // Remove the socket file if it already exists
    if Path::new(socket_path).exists() {
        std::fs::remove_file(socket_path).unwrap();
    }
    
    // Create the Unix domain socket
    let listener = UnixListener::bind(socket_path).unwrap();
    println!("Socket server listening on {}", socket_path);
    
    // Accept connections in a loop
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                // Spawn a thread to handle the connection
                thread::spawn(move || {
                    // Read the data
                    let mut buffer = Vec::new();
                    stream.read_to_end(&mut buffer).unwrap();
                    println!("Received {} bytes", buffer.len());
                    
                    // Process the data (in this case, just echo it back)
                    // In a real application, you would parse and process the message here
                    
                    // Send the reply
                    stream.write_all(&buffer).unwrap();
                    println!("Sent reply");
                });
            }
            Err(e) => {
                eprintln!("Error accepting connection: {}", e);
            }
        }
    }
}
```

## Troubleshooting

### Message Not Being Forwarded to Socket

1. Check that the topic is correctly configured with a socket path
2. Verify that the socket server is running and the socket file exists
3. Ensure that the sender's IP is in the whitelisted subnets for the topic
4. Check the logs for any socket connection or timeout errors

### Socket Server Not Receiving Messages

1. Verify that the socket path is correct and accessible
2. Check that the socket server has the necessary permissions to read/write to the socket
3. Ensure that the socket server is properly handling the connection

### Reply Not Being Sent Back

1. Verify that the socket server is sending a reply
2. Check for any timeout errors in the logs
3. Ensure that the original sender is still connected and able to receive the reply