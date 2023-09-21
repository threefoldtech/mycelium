# Packet

A `Packet` is the largest communication object between established `peers`. All communication is done
via these `packets`. The `packet` itself consists of a fixed size header, and a variable size body.
The body contains a more specific type of data.

## Packet header

The packet header has a fixed size of 4 bytes, with the following layout:

```
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|      Type     |                    Reserved                   |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

The first 8 bits are used to indicate the type of the body. `0x00` indicates a data packet, while `0x01`
indicates a control packet. The remaining 24 bits are currently reserved, and should be set to all 0.
