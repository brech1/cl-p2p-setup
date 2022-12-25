use crate::discovery::Discovery;
use discv5::enr::k256::sha2::{Digest, Sha256};
use libp2p::futures::StreamExt;
use libp2p::gossipsub::RawGossipsubMessage;
use libp2p::gossipsub::{
    FastMessageId, Gossipsub, IdentTopic as Topic, MessageAuthenticity, ValidationMode,
};
use libp2p::swarm::dial_opts::{DialOpts, PeerCondition};
use libp2p::swarm::{ConnectionLimits, SwarmBuilder, SwarmEvent};
use libp2p::{gossipsub, identity, swarm::NetworkBehaviour, PeerId};
use std::time::Duration;
mod config;
mod discovery;
mod utils;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a random PeerId
    let local_key = identity::Keypair::generate_secp256k1();
    let local_peer_id = PeerId::from(local_key.public());
    println!("Local peer id: {local_peer_id}");

    // Set up an encrypted DNS-enabled TCP Transport over the Mplex protocol.
    let transport = libp2p::tokio_development_transport(local_key.clone())?;

    let discovery = Discovery::new(&local_key).await;

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

    // Run
    loop {
        tokio::select! {
            event = swarm.select_next_some() => match event {
                SwarmEvent::Behaviour(behaviour_event) => match behaviour_event {
                    BehaviourEvent::Gossipsub(ev) => println!("Gossipsub: {ev:?}"),
                    BehaviourEvent::Discovery(discovered) => {
                        for (peer_id, _multiaddr) in discovered.peers {
                            swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);

                            let dial_opts = DialOpts::peer_id(peer_id)
                            .condition(PeerCondition::Disconnected)
                            .build();

                            swarm.dial(dial_opts).unwrap();
                        }
                    },
                },
                _ => println!("Swarm: {event:?}"),
            }
        }
    }
}
