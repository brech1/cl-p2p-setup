use discv5::enr::NodeId;
use discv5::{enr, enr::CombinedKey, Discv5, Discv5ConfigBuilder, Discv5Event, Enr};
use libp2p::futures::FutureExt;
use libp2p::identity::Keypair;
use libp2p::swarm::derive_prelude::ConnectionId;
use libp2p::swarm::{ConnectionHandler, DialError, NetworkBehaviourAction, PollParameters};
use libp2p::Multiaddr;
use libp2p::{swarm::NetworkBehaviour, PeerId};
use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::str::FromStr;
use std::task::{Context, Poll};
use std::time::Instant;
use tokio::sync::mpsc;

const EF_BOOTNODE: &str = "enr:-Ku4QHqVeJ8PPICcWk1vSn_XcSkjOkNiTg6Fmii5j6vUQgvzMc9L1goFnLKgXqBJspJjIsB91LTOleFmyWWrFVATGngBh2F0dG5ldHOIAAAAAAAAAACEZXRoMpC1MD8qAAAAAP__________gmlkgnY0gmlwhAMRHkWJc2VjcDI1NmsxoQKLVXFOhp2uX6jeT0DvvDpPcU8FWMjQdR4wMuORMhpX24N1ZHCCIyg";

pub struct Discovery {
    _discv5: Discv5,
    event_stream: EventStream,
    pub found_nodes: Vec<Enr>,
}

#[derive(Debug)]
pub struct DiscoveredPeers {
    pub peers: HashMap<PeerId, Option<Instant>>,
}

impl Discovery {
    pub async fn new(local_key: &Keypair) -> Self {
        // Setup socket
        let listen_socket = "0.0.0.0:9000".parse::<SocketAddr>().unwrap();

        // Generate ENR
        let enr_key: CombinedKey = key_from_libp2p(&local_key).unwrap();
        let mut enr_builder = enr::EnrBuilder::new("v4");

        enr_builder.ip("0.0.0.0".parse().unwrap());
        enr_builder.udp4(9000);
        enr_builder.tcp4(9000);

        let local_enr = enr_builder.build(&enr_key).unwrap();

        println!("Local enr: {local_enr}");

        // Setup default config
        let config = Discv5ConfigBuilder::new().build();

        // Create discv5 instance
        let mut discv5 = Discv5::new(local_enr, enr_key, config).unwrap();

        // Add bootnode
        let ef_bootnode_enr = Enr::from_str(EF_BOOTNODE).unwrap();
        discv5.add_enr(ef_bootnode_enr).expect("bootnode error");

        // Start the discv5 service
        discv5.start(listen_socket).await.unwrap();

        // Obtain an event stream
        let event_stream = EventStream::Awaiting(Box::pin(discv5.event_stream()));

        // Find peers
        let found_nodes: Vec<Enr> = discv5.find_node(NodeId::random()).await.unwrap();

        return Self {
            _discv5: discv5,
            event_stream,
            found_nodes,
        };
    }
}

// Get CombinedKey from libp2p Keypair
fn key_from_libp2p(key: &libp2p::core::identity::Keypair) -> Result<CombinedKey, &'static str> {
    match key {
        Keypair::Secp256k1(key) => {
            let secret = discv5::enr::k256::ecdsa::SigningKey::from_bytes(&key.secret().to_bytes())
                .expect("libp2p key must be valid");
            Ok(CombinedKey::Secp256k1(secret))
        }
        _ => Err("pair not supported"),
    }
}

impl NetworkBehaviour for Discovery {
    type ConnectionHandler = libp2p::swarm::dummy::ConnectionHandler;
    type OutEvent = DiscoveredPeers;

    fn new_handler(&mut self) -> Self::ConnectionHandler {
        libp2p::swarm::dummy::ConnectionHandler {}
    }

    // Handles the libp2p request to obtain multiaddrs for peer_id's in order to dial them.
    fn addresses_of_peer(&mut self, _peer_id: &PeerId) -> Vec<Multiaddr> {
        Vec::new()
    }

    fn inject_event(
        &mut self,
        _: PeerId,
        _: ConnectionId,
        _: <Self::ConnectionHandler as ConnectionHandler>::OutEvent,
    ) {
    }

    fn inject_dial_failure(
        &mut self,
        peer_id: Option<PeerId>,
        _handler: Self::ConnectionHandler,
        error: &DialError,
    ) {
        if let Some(_peer_id) = peer_id {
            match error {
                _ => println!("{error}"),
            }
        }
    }

    // Main execution loop to drive the behaviour
    fn poll(
        &mut self,
        cx: &mut Context,
        _: &mut impl PollParameters,
    ) -> Poll<NetworkBehaviourAction<Self::OutEvent, Self::ConnectionHandler>> {
        // Process the server event stream
        match self.event_stream {
            EventStream::Awaiting(ref mut fut) => {
                // Still awaiting the event stream, poll it
                if let Poll::Ready(event_stream) = fut.poll_unpin(cx) {
                    match event_stream {
                        Ok(stream) => {
                            println!("Discv5 event stream ready");
                            self.event_stream = EventStream::Present(stream);
                        }
                        Err(_) => {
                            println!("Discv5 event stream failed");
                            self.event_stream = EventStream::InActive;
                        }
                    }
                }
            }
            EventStream::InActive => {}
            EventStream::Present(ref mut stream) => {
                while let Poll::Ready(Some(event)) = stream.poll_recv(cx) {
                    match event {
                        _ => println!("{event:?}"),
                    }
                }
            }
        }
        Poll::Pending
    }
}

enum EventStream {
    /// Awaiting an event stream to be generated. This is required due to the poll nature of
    /// `Discovery`
    Awaiting(
        Pin<
            Box<
                dyn Future<Output = Result<mpsc::Receiver<Discv5Event>, discv5::Discv5Error>>
                    + Send,
            >,
        >,
    ),
    /// The future has completed.
    Present(mpsc::Receiver<Discv5Event>),
    // The future has failed or discv5 has been disabled. There are no events from discv5.
    InActive,
}
