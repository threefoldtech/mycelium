#!/usr/bin/env python3

import socket
import json
import sys
import os

# Path to the Unix socket
SOCKET_PATH = "/tmp/mycelium.sock"

def send_jsonrpc_request(method, params=None):
    """
    Send a JSON-RPC request to the Unix socket and return the response.
    Each connection sends one message and then closes.
    """
    # Create a Unix socket
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    
    try:
        # Connect to the socket
        sock.connect(SOCKET_PATH)
        
        # Create the JSON-RPC request
        request = {
            "jsonrpc": "2.0",
            "id": 1,
            "method": method
        }
        
        if params is not None:
            request["params"] = params
        
        # Send the request
        sock.sendall(json.dumps(request).encode('utf-8'))
        
        # Receive the response
        response = b""
        while True:
            data = sock.recv(4096)
            if not data:
                break
            response += data
        
        # Parse and return the response
        return json.loads(response.decode('utf-8'))
    
    finally:
        # Close the socket
        sock.close()

def main():
    # Check if the socket exists
    if not os.path.exists(SOCKET_PATH):
        print(f"Error: Unix socket {SOCKET_PATH} does not exist.")
        print("Make sure the mycelium daemon is running with the Unix socket enabled.")
        sys.exit(1)
    
    # Test getInfo method
    print("Testing getInfo method...")
    response = send_jsonrpc_request("getInfo")
    print(json.dumps(response, indent=2))
    print()
    
    # Test getPeers method
    print("Testing getPeers method...")
    response = send_jsonrpc_request("getPeers")
    print(json.dumps(response, indent=2))
    print()
    
    # Test addPeer method (example)
    print("Testing addPeer method...")
    response = send_jsonrpc_request("addPeer", {"endpoint": "tls://example.com:443"})
    print(json.dumps(response, indent=2))
    print()
    
    # Test getSelectedRoutes method
    print("Testing getSelectedRoutes method...")
    response = send_jsonrpc_request("getSelectedRoutes")
    print(json.dumps(response, indent=2))
    print()

if __name__ == "__main__":
    main()