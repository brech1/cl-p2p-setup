use crate::discovery::Discovery;
use discv5::enr::k256::sha2::{Digest, Sha256};
use libp2p::futures::StreamExt;
use libp2p::gossipsub::RawGossipsubMessage;
use libp2p::gossipsub::{
    FastMessageId, Gossipsub, IdentTopic as Topic, MessageAuthenticity, ValidationMode,
};
use libp2p::multiaddr::Protocol;
use libp2p::swarm::{ConnectionLimits, SwarmBuilder};
use libp2p::Multiaddr;
use libp2p::{gossipsub, identity, swarm::NetworkBehaviour, PeerId};
use std::time::Duration;
mod discovery;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a random PeerId
    let local_key = identity::Keypair::generate_secp256k1();
    let local_peer_id = PeerId::from(local_key.public());
    println!("Local peer id: {local_peer_id}");

    // Set up an encrypted DNS-enabled TCP Transport over the Mplex protocol.
    let transport = libp2p::tokio_development_transport(local_key.clone())?;

    let discovery = Discovery::new(&local_key).await;

    let mut nodes_multiaddr: Vec<Multiaddr> = Vec::new();

    for node in discovery.found_nodes.clone() {
        if let Some(ip) = node.ip4() {
            if let Some(tcp) = node.tcp4() {
                if tcp == 9000 {
                    let mut multiaddr: Multiaddr = ip.into();
                    multiaddr.push(Protocol::Tcp(tcp));
                    nodes_multiaddr.push(multiaddr);
                }
            }
        }
    }

    let fast_gossip_message_id =
        |message: &RawGossipsubMessage| FastMessageId::from(&Sha256::digest(&message.data)[..8]);

    // Set a custom gossipsub configuration
    let gossipsub_config = gossipsub::GossipsubConfigBuilder::default()
        .max_transmit_size(10 * 1_048_576)
        .fanout_ttl(Duration::from_secs(60))
        .heartbeat_interval(Duration::from_millis(10_000))
        .validation_mode(ValidationMode::Anonymous)
        .fast_message_id_fn(fast_gossip_message_id)
        .fanout_ttl(Duration::from_secs(60))
        .history_length(12)
        .max_messages_per_rpc(Some(500))
        .build()
        .expect("Valid config");

    // build a gossipsub network behaviour
    let mut gossipsub = Gossipsub::new(MessageAuthenticity::Anonymous, gossipsub_config)
        .expect("Correct configuration");

    // Create a Gossipsub topic
    let topic = Topic::new("/eth2/4a26c58b/beacon_block/ssz_snappy");

    // subscribes to our topic
    gossipsub.subscribe(&topic)?;

    // We create a custom network behaviour that combines Gossipsub and Mdns.
    #[derive(NetworkBehaviour)]
    struct Behaviour {
        gossipsub: Gossipsub,
        discovery: Discovery,
    }

    let behaviour = {
        Behaviour {
            gossipsub,
            discovery,
        }
    };

    // Create a Swarm to manage peers and events
    let mut swarm = SwarmBuilder::with_tokio_executor(transport, behaviour, local_peer_id)
        .notify_handler_buffer_size(std::num::NonZeroUsize::new(7).expect("Not zero"))
        .connection_event_buffer_size(64)
        .connection_limits(ConnectionLimits::default())
        .build();

    // Listen
    swarm.listen_on("/ip4/0.0.0.0/tcp/9000".parse()?)?;

    for node in nodes_multiaddr.clone() {
        swarm.dial(node)?;
    }

    // swarm.dial(nodes_multiaddr[1].clone())?;

    // Run
    loop {
        tokio::select! {
            event = swarm.next() => match event {
                _ => println!("Swarm: {event:?}"),
            }
        }
    }
}
