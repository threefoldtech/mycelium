# Data packet

A `data packet` contains user specified data. This can be any data, as long as the sender and receiver
both understand what it is, without further help. Intermediate hops, which route the data have sufficient
information with the header to know where to forward the packet. In practice, the data will be encrypted
to avoid eavesdropping by intermediate hops.

## Packet header

The packet header has a fixed size of 36 bytes, with the following layout:

```
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|    Reserved   |             Length            |   Hop Limit   |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                                                               |
+                                                               +
|                                                               |
+                           Source IP                           +
|                                                               |
+                                                               +
|                                                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                                                               |
+                                                               +
|                                                               |
+                         Destination IP                        +
|                                                               |
+                                                               +
|                                                               |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

The first 8 bits are reserved and must be set to 0.

The next 16 bits are used to specify the length of the body. It is expected that
the actual length of a packet does not exceed 65K right now, and overhead related
to encryption should be handled by the client before sending the packet.

The next byte is the hop-limit. Every node decrements this value by 1 before sending
the packet. If a node decrements this value to 0, the packet is discarded.

The next 16 bytes contain the sender IP address.

The final 16 bytes contain the destination IP address.

## Body

Following the header is a variable length body. The protocol does not have any requirements for the
body, and the only requirement imposed is that the body is as long as specified in the header length
field. It is technically legal according to the protocol to transmit a data packet without a body,
i.e. a body length of 0. This is useless however, as there will not be any data to interpret.
