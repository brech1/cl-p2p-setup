# Consensus Layer P2P

This project is a basic setup for a consensus layer peer-to-peer connection, as specified in the [consensus layer specifications](https://github.com/ethereum/consensus-specs/blob/v1.2.0/specs/phase0/p2p-interface.md) of the peer-to-peer interface. 

I also made this brief [post](https://mirror.xyz/brechy.eth/gE8NFWIQ6sCcW7ayjy-79Uq6UDLKJ5UCvbBVA2XrBNg) for my initial research on the topic which you may find complementary for the code.

It is based on the [lighthouse](https://github.com/sigp/lighthouse) client beacon chain networking and examples of basic implementations for both discovery and gossipsub connections, featuring:

- [libp2p](https://github.com/libp2p/rust-libp2p)
- [discv5](https://github.com/sigp/discv5)
- [tokio](https://github.com/tokio-rs/tokio)

Established connections are dropped by peers due to not having the correct [eth2](https://github.com/ethereum/consensus-specs/blob/v1.2.0/specs/phase0/p2p-interface.md#eth2-field) field in the ENR. Working on it!

There's also some work to be made for message processing using:

- [SSZ](https://ethereum.org/en/developers/docs/data-structures-and-encoding/ssz/)
- [Snappy](https://en.wikipedia.org/wiki/Snappy_(compression))
