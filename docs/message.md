# Message subsystem

The message subsystem can be used to send arbitrary length messages to receivers. A receiver is any
other node in the network. It can be identified both by its public key, or an IP address in its announced
range. The message subsystem can be interacted with both via the HTTP API, which is
[documented here](./api.yaml), or via the `mycelium` binary. By default, the messages do not interpret
the data in any way. When using the binary, the message is slightly modified to include an optional
topic at the start of the message. Note that in the HTTP API, all messages are encoded in base64. This
might make it difficult to consume these messages without additional tooling.

## Curl examples

These examples assume you have at least 2 nodes running, and that they are both part of the same network.

Send a message on node1, waiting up to 2 minutes for a possible reply:

```bash
curl -v -H 'Content-Type: application/json' -d '{"dst": {"pk": "bb39b4a3a4efd70f3e05e37887677e02efbda14681d0acd3882bc0f754792c32"}, "payload": "xuV+"}' http://localhost:8989/api/v1/messages\?reply_timeout\=120
```

Listen for a message on node2. Note that messages received while nothing is listening are added to
a queue for later consumption. Wait for up to 1 minute.

```bash
curl -v http://localhost:8989/api/v1/messages\?timeout\=60\
```

The system will (immediately) receive our previously sent message:

```json
{"id":"e47b25063912f4a9","srcIp":"34f:b680:ba6e:7ced:355f:346f:d97b:eecb","srcPk":"955bf6bea5e1150fd8e270c12e5b2fc08f08f7c5f3799d10550096cc137d671b","dstIp":"2e4:9ace:9252:630:beee:e405:74c0:d876","dstPk":"bb39b4a3a4efd70f3e05e37887677e02efbda14681d0acd3882bc0f754792c32","payload":"xuV+"}
```

To send a reply, we can post a message on the reply path, with the received message `id` (still on
node2):

```bash
curl -H 'Content-Type: application/json' -d '{"dst": {"pk":"955bf6bea5e1150fd8e270c12e5b2fc08f08f7c5f3799d10550096cc137d671b"}, "payload": "xuC+"}' http://localhost:8989/api/v1/messages/reply/e47b25063912f4a9
```

If you did this fast enough, the initial sender (node1) will now receive the reply.

## Mycelium binary examples

As explained above, while using the binary the message is slightly modified to insert the optional
topic. As such, when using the binary to send messages, it is suggested to make sure the receiver is
also using the binary to listen for messages. The options discussed here are not covering all possibilities,
use the `--help` flag (`mycelium message send --help` and `mycelium message receive --help`) for a
full overview.

Once again, send a message. This time using a topic (example.topic). Note that there are no constraints
on what a valid topic is, other than that it is valid UTF-8, and at most 255 bytes in size. The `--wait`
flag can be used to indicate that we are waiting for a reply. If it is set, we can also use an additional
`--timeout` flag to govern exactly how long (in seconds) to wait for. The default is to wait forever.

```bash
mycelium message send 2e4:9ace:9252:630:beee:e405:74c0:d876 'this is a message' -t example.topic --wait
```

On the second node, listen for messages with this topic. If a different topic is used, the previous
message won't be received. If no topic is set, all messages are received. An optional timeout flag
can be specified, which indicates how long to wait for. Absence of this flag will cause the binary
to wait forever.

```bash
mycelium message receive -t example.topic
```

Again, if the previous command was executed a message will be received immediately:

```json
{"id":"4a6c956e8d36381f","topic":"example.topic","srcIp":"34f:b680:ba6e:7ced:355f:346f:d97b:eecb","srcPk":"955bf6bea5e1150fd8e270c12e5b2fc08f08f7c5f3799d10550096cc137d671b","dstIp":"2e4:9ace:9252:630:beee:e405:74c0:d876","dstPk":"bb39b4a3a4efd70f3e05e37887677e02efbda14681d0acd3882bc0f754792c32","payload":"this is a message"}
```

And once again, we can use the ID from this message to reply to the original sender, who might be waiting
for this reply (notice we used the hex encoded public key to identify the receiver here, rather than an IP):

```bash
mycelium message send 955bf6bea5e1150fd8e270c12e5b2fc08f08f7c5f3799d10550096cc137d671b "this is a reply" --reply-to 4a6c956e8d36381f
```
