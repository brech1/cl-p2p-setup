# CL P2P Setup

Minimal setup for a consensus layer peer-to-peer connection. Complementary of [this post](https://mirror.xyz/brechy.eth/gE8NFWIQ6sCcW7ayjy-79Uq6UDLKJ5UCvbBVA2XrBNg). 

Featuring:

- [libp2p](https://github.com/libp2p/rust-libp2p)
- [discv5](https://github.com/sigp/discv5)
- [tokio](https://github.com/tokio-rs/tokio)

And based on some of the amazing work on the [lighthouse](https://github.com/sigp/lighthouse) beacon chain client.

There's some work to be made on:

- Dialing
- Gossipsub filtering
- Discv5 config
- Snappy and SSZ
