# Mycelium API OpenRPC Specification

This repository contains an OpenRPC specification for the Mycelium API. The specification describes the available methods, parameters, and response formats for interacting with a Mycelium node.

## What is OpenRPC?

[OpenRPC](https://open-rpc.org/) is a specification for defining JSON-RPC 2.0 APIs, similar to how OpenAPI/Swagger is used for REST APIs. While the Mycelium API is actually a REST API (not JSON-RPC), this OpenRPC-style specification provides a standardized way to document the API's capabilities.

## Specification Overview

The specification (`mycelium-api-openrpc.json`) includes:

- Basic API information (version, title, description)
- Server information
- Detailed documentation for all available methods
- Schema definitions for request and response objects

## Available Methods

The Mycelium API provides the following methods:

### Node Administration

- `getInfo` - Get general info about the node
- `getPeers` - Get the stats of current known peers
- `addPeer` - Add a new peer to the system
- `deletePeer` - Remove an existing peer from the system

### Routing

- `getSelectedRoutes` - List all currently selected routes
- `getFallbackRoutes` - List all active fallback routes

### Public Key Management

- `getPubkeyFromIp` - Get public key from IP

### Messaging

- `getMessage` - Get a message (with optional peeking and timeout)
- `pushMessage` - Push a new message
- `replyMessage` - Reply to a message
- `messageStatus` - Get the status of a message

## Using the Specification

This OpenRPC specification can be used to:

1. Generate client libraries for various programming languages
2. Create interactive documentation
3. Set up mock servers for testing
4. Validate API requests and responses

## REST API Endpoints

The actual REST API endpoints corresponding to these methods are:

| Method | HTTP Endpoint | HTTP Method |
|--------|--------------|-------------|
| getInfo | /api/v1/admin | GET |
| getPeers | /api/v1/admin/peers | GET |
| addPeer | /api/v1/admin/peers | POST |
| deletePeer | /api/v1/admin/peers/{endpoint} | DELETE |
| getSelectedRoutes | /api/v1/admin/routes/selected | GET |
| getFallbackRoutes | /api/v1/admin/routes/fallback | GET |
| getPubkeyFromIp | /api/v1/pubkey/{ip} | GET |
| getMessage | /api/v1/messages | GET |
| pushMessage | /api/v1/messages | POST |
| replyMessage | /api/v1/messages/reply/{id} | POST |
| messageStatus | /api/v1/messages/status/{id} | GET |

## Example Usage

Here's an example of how to use the Mycelium API to add a new peer:

```bash
curl -X POST http://localhost:9651/api/v1/admin/peers \
  -H "Content-Type: application/json" \
  -d '{"endpoint": "tcp://192.168.1.100:9651"}'
```

And to get information about the node:

```bash
curl -X GET http://localhost:9651/api/v1/admin
```

## Tools for Working with OpenRPC

- [OpenRPC Playground](https://playground.open-rpc.org/) - Interactive editor and documentation viewer
- [OpenRPC Generator](https://github.com/open-rpc/generator) - Generate clients, servers, documentation, and more
- [OpenRPC Meta-Schema](https://github.com/open-rpc/meta-schema) - JSON Schema for validating OpenRPC documents

## Notes

This specification is based on the Mycelium API implementation as of version 0.5.7. The actual API implementation may have additional features or changes not reflected in this specification.