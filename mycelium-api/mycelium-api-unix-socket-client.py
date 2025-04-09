#!/usr/bin/env python3
"""
Example script demonstrating how to use the Mycelium Unix Socket API.
This script provides examples of common operations using the API endpoints
defined in the OpenRPC specification.
"""

import socket
import json
import base64
import argparse
import sys
from typing import Dict, Any, Optional, List, Union

# Path to the Unix socket
DEFAULT_SOCKET_PATH = "/tmp/mycelium.sock"

class MyceliumUnixSocketClient:
    """Client for interacting with the Mycelium API via Unix socket."""

    def __init__(self, socket_path: str = DEFAULT_SOCKET_PATH):
        """Initialize the client with the Unix socket path."""
        self.socket_path = socket_path

    def _send_jsonrpc_request(self, method: str, params: Optional[Dict[str, Any]] = None) -> Dict[str, Any]:
        """
        Send a JSON-RPC request to the Unix socket and return the response.
        Each connection sends one message and then closes.
        """
        # Create a Unix socket
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        
        try:
            # Connect to the socket
            sock.connect(self.socket_path)
            
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
            result = json.loads(response.decode('utf-8'))
            
            # Check for errors
            if "error" in result:
                error = result["error"]
                raise Exception(f"JSON-RPC error {error['code']}: {error['message']}")
                
            return result.get("result", {})
            
        finally:
            # Close the socket
            sock.close()

    def get_info(self) -> Dict[str, Any]:
        """Get general info about the node."""
        return self._send_jsonrpc_request("getInfo")

    def get_peers(self) -> List[Dict[str, Any]]:
        """Get the stats of current known peers."""
        return self._send_jsonrpc_request("getPeers")

    def add_peer(self, endpoint: str) -> bool:
        """Add a new peer to the system."""
        return self._send_jsonrpc_request("addPeer", {"endpoint": endpoint})

    def delete_peer(self, endpoint: str) -> bool:
        """Remove an existing peer from the system."""
        return self._send_jsonrpc_request("deletePeer", {"endpoint": endpoint})

    def get_selected_routes(self) -> List[Dict[str, Any]]:
        """List all currently selected routes."""
        return self._send_jsonrpc_request("getSelectedRoutes")

    def get_fallback_routes(self) -> List[Dict[str, Any]]:
        """List all active fallback routes."""
        return self._send_jsonrpc_request("getFallbackRoutes")

    def get_pubkey_from_ip(self, ip: str) -> Dict[str, str]:
        """Get public key from IP."""
        return self._send_jsonrpc_request("getPubkeyFromIp", {"ip": ip})

    def get_message(
        self, 
        peek: bool = False, 
        timeout: int = 0, 
        topic: Optional[bytes] = None
    ) -> Dict[str, Any]:
        """
        Get a message from the queue.
        
        Args:
            peek: Whether to peek at the message without removing it
            timeout: Number of seconds to wait for a message if none is available
            topic: Optional filter for message topic
        """
        params = {"peek": peek, "timeout": timeout}
        if topic:
            params["topic"] = base64.b64encode(topic).decode("ascii")
            
        return self._send_jsonrpc_request("getMessage", params)

    def push_message(
        self,
        destination: Union[Dict[str, str], str],
        payload: bytes,
        topic: Optional[bytes] = None,
        reply_timeout: Optional[int] = None
    ) -> Dict[str, Any]:
        """
        Push a new message.
        
        Args:
            destination: Either {"ip": "ip_address"} or {"pk": "public_key"}
            payload: Message payload
            topic: Optional message topic
            reply_timeout: Optional timeout in seconds to wait for a reply
        """
        # Convert string destination to dict if needed
        if isinstance(destination, str):
            if destination.count(":") >= 2:  # Likely an IPv6 address
                dst = {"ip": destination}
            else:
                try:
                    # Try to parse as IPv4
                    parts = destination.split(".")
                    if len(parts) == 4 and all(0 <= int(p) <= 255 for p in parts):
                        dst = {"ip": destination}
                    else:
                        dst = {"pk": destination}
                except (ValueError, IndexError):
                    dst = {"pk": destination}
        else:
            dst = destination
            
        params = {
            "reply_timeout": reply_timeout,
            "message": {
                "dst": dst,
                "payload": base64.b64encode(payload).decode("ascii")
            }
        }
        
        if topic:
            params["message"]["topic"] = base64.b64encode(topic).decode("ascii")
            
        return self._send_jsonrpc_request("pushMessage", params)

    def reply_message(
        self,
        message_id: str,
        destination: Union[Dict[str, str], str],
        payload: bytes,
        topic: Optional[bytes] = None
    ) -> bool:
        """
        Reply to a message.
        
        Args:
            message_id: ID of the message to reply to
            destination: Either {"ip": "ip_address"} or {"pk": "public_key"}
            payload: Message payload
            topic: Optional message topic
        """
        # Convert string destination to dict if needed
        if isinstance(destination, str):
            if destination.count(":") >= 2:  # Likely an IPv6 address
                dst = {"ip": destination}
            else:
                try:
                    # Try to parse as IPv4
                    parts = destination.split(".")
                    if len(parts) == 4 and all(0 <= int(p) <= 255 for p in parts):
                        dst = {"ip": destination}
                    else:
                        dst = {"pk": destination}
                except (ValueError, IndexError):
                    dst = {"pk": destination}
        else:
            dst = destination
            
        params = {
            "id": message_id,
            "message": {
                "dst": dst,
                "payload": base64.b64encode(payload).decode("ascii")
            }
        }
        
        if topic:
            params["message"]["topic"] = base64.b64encode(topic).decode("ascii")
            
        return self._send_jsonrpc_request("replyMessage", params)

    def message_status(self, message_id: str) -> Dict[str, Any]:
        """Get the status of a message."""
        return self._send_jsonrpc_request("messageStatus", {"id": message_id})


def print_json(data: Any) -> None:
    """Print data as formatted JSON."""
    print(json.dumps(data, indent=2))


def main():
    """Main function to parse arguments and execute commands."""
    parser = argparse.ArgumentParser(description="Mycelium Unix Socket API Client")
    parser.add_argument("--socket", default=DEFAULT_SOCKET_PATH, help="Path to the Unix socket")
    
    subparsers = parser.add_subparsers(dest="command", help="Command to execute")
    
    # Info command
    subparsers.add_parser("info", help="Get node info")
    
    # Peers commands
    peers_parser = subparsers.add_parser("peers", help="Peer operations")
    peers_subparsers = peers_parser.add_subparsers(dest="peer_command")
    peers_subparsers.add_parser("list", help="List peers")
    
    add_peer_parser = peers_subparsers.add_parser("add", help="Add a peer")
    add_peer_parser.add_argument("endpoint", help="Peer endpoint")
    
    del_peer_parser = peers_subparsers.add_parser("delete", help="Delete a peer")
    del_peer_parser.add_argument("endpoint", help="Peer endpoint")
    
    # Routes commands
    routes_parser = subparsers.add_parser("routes", help="Route operations")
    routes_subparsers = routes_parser.add_subparsers(dest="route_command")
    routes_subparsers.add_parser("selected", help="List selected routes")
    routes_subparsers.add_parser("fallback", help="List fallback routes")
    
    # Pubkey command
    pubkey_parser = subparsers.add_parser("pubkey", help="Get public key from IP")
    pubkey_parser.add_argument("ip", help="IP address")
    
    # Message commands
    msg_parser = subparsers.add_parser("messages", help="Message operations")
    msg_subparsers = msg_parser.add_subparsers(dest="message_command")
    
    get_msg_parser = msg_subparsers.add_parser("get", help="Get a message")
    get_msg_parser.add_argument("--peek", action="store_true", help="Peek at message without removing")
    get_msg_parser.add_argument("--timeout", type=int, default=0, help="Timeout in seconds")
    get_msg_parser.add_argument("--topic", help="Message topic (will be base64 encoded)")
    
    push_msg_parser = msg_subparsers.add_parser("push", help="Push a message")
    push_msg_parser.add_argument("destination", help="Destination (IP or public key)")
    push_msg_parser.add_argument("payload", help="Message payload")
    push_msg_parser.add_argument("--topic", help="Message topic")
    push_msg_parser.add_argument("--reply-timeout", type=int, help="Reply timeout in seconds")
    
    reply_msg_parser = msg_subparsers.add_parser("reply", help="Reply to a message")
    reply_msg_parser.add_argument("id", help="Message ID")
    reply_msg_parser.add_argument("destination", help="Destination (IP or public key)")
    reply_msg_parser.add_argument("payload", help="Message payload")
    reply_msg_parser.add_argument("--topic", help="Message topic")
    
    status_msg_parser = msg_subparsers.add_parser("status", help="Get message status")
    status_msg_parser.add_argument("id", help="Message ID")
    
    args = parser.parse_args()
    
    if not args.command:
        parser.print_help()
        return
    
    client = MyceliumUnixSocketClient(args.socket)
    
    try:
        if args.command == "info":
            print_json(client.get_info())
            
        elif args.command == "peers":
            if not args.peer_command:
                print("Error: Missing peer command")
                return
                
            if args.peer_command == "list":
                print_json(client.get_peers())
            elif args.peer_command == "add":
                success = client.add_peer(args.endpoint)
                print(f"Peer {'added successfully' if success else 'addition failed'}")
            elif args.peer_command == "delete":
                success = client.delete_peer(args.endpoint)
                print(f"Peer {'deleted successfully' if success else 'deletion failed'}")
                
        elif args.command == "routes":
            if not args.route_command:
                print("Error: Missing route command")
                return
                
            if args.route_command == "selected":
                print_json(client.get_selected_routes())
            elif args.route_command == "fallback":
                print_json(client.get_fallback_routes())
                
        elif args.command == "pubkey":
            print_json(client.get_pubkey_from_ip(args.ip))
            
        elif args.command == "messages":
            if not args.message_command:
                print("Error: Missing message command")
                return
                
            if args.message_command == "get":
                topic_bytes = args.topic.encode("utf-8") if args.topic else None
                print_json(client.get_message(args.peek, args.timeout, topic_bytes))
                
            elif args.message_command == "push":
                payload_bytes = args.payload.encode("utf-8")
                topic_bytes = args.topic.encode("utf-8") if args.topic else None
                print_json(client.push_message(
                    args.destination, 
                    payload_bytes, 
                    topic_bytes, 
                    args.reply_timeout
                ))
                
            elif args.message_command == "reply":
                payload_bytes = args.payload.encode("utf-8")
                topic_bytes = args.topic.encode("utf-8") if args.topic else None
                success = client.reply_message(
                    args.id, 
                    args.destination, 
                    payload_bytes, 
                    topic_bytes
                )
                print(f"Reply {'sent successfully' if success else 'failed'}")
                
            elif args.message_command == "status":
                print_json(client.message_status(args.id))
                
    except Exception as e:
        print(f"Error: {e}")
        sys.exit(1)


if __name__ == "__main__":
    main()