use crate::config::{DUPLICATE_CACHE_TIME, GOSSIP_MAX_SIZE_BELLATRIX};
use crate::discovery::Discovery;
use crate::peer_manager::PeerManager;
use crate::rpc::protocol::InboundRequest;
use crate::rpc::{ReqId, RequestId, RPC};
use libp2p::futures::StreamExt;
use libp2p::gossipsub::subscription_filter::AllowAllSubscriptionFilter;
use libp2p::gossipsub::{
    DataTransform, Gossipsub, GossipsubMessage, IdentTopic, MessageAuthenticity, MessageId,
    RawGossipsubMessage, TopicHash, ValidationMode,
};
use libp2p::swarm::{ConnectionLimits, NetworkBehaviour, SwarmBuilder, SwarmEvent};
use libp2p::{
    core, dns, gossipsub, identify, identity, mplex, noise, tcp, websocket, yamux, PeerId,
    Transport,
};
use log::{debug, info, warn};
use sha2::{Digest, Sha256};
use snap::raw::{decompress_len, Decoder, Encoder};
use std::io::{Error, ErrorKind};
use std::time::Duration;
mod chain;
mod config;
mod discovery;
mod enr;
mod peer_manager;
mod rpc;
use crate::rpc::methods::{
    EnrAttestationBitfield, EnrSyncCommitteeBitfield, MetaData, RPCCodedResponse, RPCResponse,
};

const PEER_DATA_FILE: &str = "peer_data.json";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();
    // Create a random PeerId
    let local_key = identity::Keypair::generate_secp256k1();
    let local_peer_id = PeerId::from(local_key.public());

    info!("Local peer id: {local_peer_id}");

    // Set up an encrypted DNS-enabled TCP Transport over the Mplex protocol.
    let transport = build_transport(&local_key)?;

    let discovery = Discovery::new(&local_key).await;

    let target_num_peers = 16;
    let mut peer_manager = PeerManager::new(target_num_peers);
    peer_manager.load_peer_data(PEER_DATA_FILE);

    fn prefix(prefix: [u8; 4], message: &GossipsubMessage) -> Vec<u8> {
        let topic_bytes = message.topic.as_str().as_bytes();
        let topic_len_bytes = topic_bytes.len().to_le_bytes();
        let mut vec = Vec::with_capacity(
            prefix.len() + topic_len_bytes.len() + topic_bytes.len() + message.data.len(),
        );
        vec.extend_from_slice(&prefix);
        vec.extend_from_slice(&topic_len_bytes);
        vec.extend_from_slice(topic_bytes);
        vec.extend_from_slice(&message.data);
        vec
    }

    let gossip_message_id = move |message: &GossipsubMessage| {
        MessageId::from(
            &Sha256::digest(prefix(MESSAGE_DOMAIN_VALID_SNAPPY, message).as_slice())[..20],
        )
    };

    // Set a custom gossipsub configuration
    let gossipsub_config = gossipsub::GossipsubConfigBuilder::default()
        .max_transmit_size(GOSSIP_MAX_SIZE_BELLATRIX)
        .mesh_n(5)
        .mesh_n_low(3)
        .mesh_outbound_min(2)
        .mesh_n_high(10)
        .gossip_lazy(3)
        .fanout_ttl(Duration::from_secs(60))
        .heartbeat_interval(Duration::from_millis(700))
        .validate_messages()
        .validation_mode(ValidationMode::Anonymous)
        .duplicate_cache_time(DUPLICATE_CACHE_TIME)
        .fanout_ttl(Duration::from_secs(60))
        .history_length(12)
        .max_messages_per_rpc(Some(500))
        .allow_self_origin(true)
        .message_id_fn(gossip_message_id)
        .build()
        .expect("Valid config");

    // build a gossipsub network behaviour
    let mut gossipsub = Gossipsub::new_with_transform(
        MessageAuthenticity::Anonymous,
        gossipsub_config,
        None,
        SnappyTransform::new(),
    )?;

    // Create a Gossipsub topic
    let topic = IdentTopic::new("/eth2/4a26c58b/beacon_block/ssz_snappy");

    // subscribes to our topic
    gossipsub.subscribe(&topic)?;

    let identify = identify::Behaviour::new(
        identify::Config::new("".into(), local_key.public()).with_cache_size(0),
    );

    let rpc: RPC<RequestId<()>> = RPC::new();

    // We create a custom network behaviour that combines Gossipsub and Discv5.
    #[derive(NetworkBehaviour)]
    struct Behaviour<AppReqId: ReqId> {
        gossipsub: Gossipsub<SnappyTransform, AllowAllSubscriptionFilter>,
        discovery: Discovery,
        rpc: RPC<RequestId<AppReqId>>,
        identify: identify::Behaviour,
        peer_manager: PeerManager,
    }

    let behaviour = {
        Behaviour {
            gossipsub,
            discovery,
            rpc,
            identify,
            peer_manager,
        }
    };

    // Create a Swarm to manage peers and events
    let mut swarm = SwarmBuilder::with_tokio_executor(transport, behaviour, local_peer_id)
        .notify_handler_buffer_size(std::num::NonZeroUsize::new(7).expect("Not zero"))
        .connection_event_buffer_size(64)
        .connection_limits(
            ConnectionLimits::default()
                .with_max_pending_incoming(Some(64))
                .with_max_pending_outgoing(Some(32))
                .with_max_established_per_peer(Some(10)),
        )
        .build();

    // Listen
    swarm.listen_on("/ip4/0.0.0.0/tcp/9000".parse()?)?;

    let time_to_stop = std::time::Instant::now() + std::time::Duration::from_secs(60 * 3);
    // Run
    while std::time::Instant::now() < time_to_stop {
        tokio::select! {
            event = swarm.select_next_some() => match event {
                SwarmEvent::Behaviour(behaviour_event) => match behaviour_event {
                    BehaviourEvent::Gossipsub(gs) =>
                    match gs {
                        gossipsub::GossipsubEvent::Message { propagation_source: _, message_id: _, message } => debug!("Gossipsub Message: {:#?}", message),
                        _ => ()
                    },
                    BehaviourEvent::Discovery(discovered) => {
                        debug!("Discovery Event: {:#?}", &discovered);
                            swarm.behaviour_mut().peer_manager.add_peers(discovered.peers);
                    },
                    BehaviourEvent::Rpc(rpc_message) =>{
                        debug!("RPC message: {:#?}", rpc_message);
                        match rpc_message.event {
                        Ok(received) => match received {
                        rpc::RPCReceived::Request(substream, inbound_req) => {
                            debug!("RPC Request: {:#?}", inbound_req);
                            match inbound_req {
                                InboundRequest::Status(status)=>{
                                    swarm.behaviour_mut().rpc.send_response(rpc_message.peer_id, (rpc_message.conn_id,substream), RPCCodedResponse::Success(RPCResponse::Status(status)));
                                },
                                InboundRequest::Ping(_) => {
                                    swarm.behaviour_mut().rpc.send_response(rpc_message.peer_id, (rpc_message.conn_id,substream), RPCCodedResponse::Success(RPCResponse::Pong(rpc::methods::Ping {
                                        data: 0,
                                    })));
                                },
                                InboundRequest::MetaData => {
                                    swarm.behaviour_mut().rpc.send_response(rpc_message.peer_id, (rpc_message.conn_id,substream), RPCCodedResponse::Success(RPCResponse::MetaData(MetaData{
                                        seq_number: 0,
                                        attnets: EnrAttestationBitfield::default(),
                                        syncnets: EnrSyncCommitteeBitfield::default(),
                                    })));
                                },
                                _ => {
                                    swarm.behaviour_mut().rpc.send_response(rpc_message.peer_id, (rpc_message.conn_id,substream), RPCCodedResponse::Error);
                                },
                            }
                        },
                        rpc::RPCReceived::Response(_, _) => todo!(),
                        },
                        Err(e) =>  warn!("Error in RPC quest handling: {}", e),
                    }},
                    BehaviourEvent::Identify(ev) => {
                        debug!("identify: {:#?}", ev);
                        match ev {
                            libp2p::identify::Event::Received { peer_id, info, .. } => {
                                swarm.behaviour_mut().peer_manager.add_peer_identity(peer_id, info);
                            }
                            _ => {}
                        }
                    },
                    BehaviourEvent::PeerManager(ev) => {
                        debug!("PeerManager event: {:#?}", ev);
                        match ev {
                            peer_manager::PeerManagerEvent::DiscoverPeers(num_peers) => {
                                swarm.behaviour_mut().discovery.set_peers_to_discover(num_peers as usize);
                            },
                            peer_manager::PeerManagerEvent::DialPeers(peer_ids) => {
                                for peer_id in peer_ids {
                                    swarm.behaviour_mut().gossipsub.add_explicit_peer(&peer_id);
                                }
                            },
                        }
                    },
                },
                SwarmEvent::ConnectionClosed { peer_id, endpoint, num_established, cause } => {
                    debug!("ConnectionClosed: Cause {cause:?} - PeerId: {peer_id:?} - NumEstablished: {num_established:?} - Endpoint: {endpoint:?}");
                },
                SwarmEvent::ConnectionEstablished { peer_id, endpoint, num_established, .. } => {
                    debug!("ConnectionEstablished: PeerId: {peer_id:?} - NumEstablished: {num_established:?} - Endpoint: {endpoint:?}");
                },
                _ => debug!("Swarm: {event:?}"),
            },
            _ = tokio::time::sleep(Duration::from_secs(1)) => {
                info!("Connected peers: {}", swarm.behaviour_mut().gossipsub.all_peers().collect::<Vec<_>>().len());
            }
        }
    }

    swarm.behaviour().peer_manager.log_identities();
    swarm.behaviour().peer_manager.log_metrics();
    swarm
        .behaviour_mut()
        .peer_manager
        .save_peer_data(PEER_DATA_FILE);
    Ok(())
}

pub fn build_transport(
    keypair: &identity::Keypair,
) -> std::io::Result<core::transport::Boxed<(PeerId, core::muxing::StreamMuxerBox)>> {
    let transport = {
        let dns_tcp = dns::TokioDnsConfig::system(tcp::tokio::Transport::new(
            tcp::Config::new().nodelay(true),
        ))?;
        let ws_dns_tcp = websocket::WsConfig::new(dns::TokioDnsConfig::system(
            tcp::tokio::Transport::new(tcp::Config::new().nodelay(true)),
        )?);
        dns_tcp.or_transport(ws_dns_tcp)
    };

    let mut mplex_config = mplex::MplexConfig::new();
    mplex_config.set_max_buffer_size(256);
    mplex_config.set_max_buffer_behaviour(mplex::MaxBufferBehaviour::Block);

    let mut yamux_config = yamux::YamuxConfig::default();
    yamux_config.set_window_update_mode(yamux::WindowUpdateMode::on_read());

    Ok(transport
        .upgrade(core::upgrade::Version::V1)
        .authenticate(generate_noise_config(keypair))
        .multiplex(core::upgrade::SelectUpgrade::new(
            yamux_config,
            mplex_config,
        ))
        .timeout(std::time::Duration::from_secs(100))
        .boxed())
}

const MESSAGE_DOMAIN_VALID_SNAPPY: [u8; 4] = [1, 0, 0, 0];

pub struct SnappyTransform {
    max_size_per_message: usize,
}

impl SnappyTransform {
    pub fn new() -> Self {
        SnappyTransform {
            max_size_per_message: GOSSIP_MAX_SIZE_BELLATRIX,
        }
    }
}

impl DataTransform for SnappyTransform {
    fn inbound_transform(
        &self,
        raw_message: RawGossipsubMessage,
    ) -> Result<GossipsubMessage, std::io::Error> {
        let len = decompress_len(&raw_message.data)?;
        if len > self.max_size_per_message {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "ssz_snappy decoded data > GOSSIP_MAX_SIZE",
            ));
        }

        let mut decoder = Decoder::new();
        let decompressed_data = decoder.decompress_vec(&raw_message.data)?;

        Ok(GossipsubMessage {
            source: raw_message.source,
            data: decompressed_data,
            sequence_number: raw_message.sequence_number,
            topic: raw_message.topic,
        })
    }

    fn outbound_transform(
        &self,
        _topic: &TopicHash,
        data: Vec<u8>,
    ) -> Result<Vec<u8>, std::io::Error> {
        println!("Outbound transform: {}", _topic);
        if data.len() > self.max_size_per_message {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "ssz_snappy Encoded data > GOSSIP_MAX_SIZE",
            ));
        }
        let mut encoder = Encoder::new();
        encoder.compress_vec(&data).map_err(Into::into)
    }
}

fn generate_noise_config(
    identity_keypair: &identity::Keypair,
) -> noise::NoiseAuthenticated<noise::XX, noise::X25519Spec, ()> {
    let static_dh_keys = noise::Keypair::<noise::X25519Spec>::new()
        .into_authentic(identity_keypair)
        .expect("signing can fail only once during starting a node");
    noise::NoiseConfig::xx(static_dh_keys).into_authenticated()
}
