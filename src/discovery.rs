use crate::config::BOOTNODE;
use crate::enr::{build_enr, EnrAsPeerId, EnrForkId};
use discv5::enr::NodeId;
use discv5::{enr::CombinedKey, Discv5, Discv5ConfigBuilder, Discv5Event, Enr};
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use libp2p::futures::FutureExt;
use libp2p::identity::Keypair;
use libp2p::multiaddr::Protocol;
use libp2p::swarm::{NetworkBehaviour, NetworkBehaviourAction, PollParameters};
use libp2p::{Multiaddr, PeerId};
use std::collections::HashMap;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::str::FromStr;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

pub struct Discovery {
    discv5: Discv5,
    _enr: Enr,
    event_stream: EventStream,
    multiaddr_map: HashMap<PeerId, Multiaddr>,
    peers_future: FuturesUnordered<std::pin::Pin<Box<dyn Future<Output = DiscResult> + Send>>>,
    started: bool,
    poll_count: u64,
}

type DiscResult = Result<Vec<discv5::enr::Enr<CombinedKey>>, discv5::QueryError>;

#[derive(Debug, Clone)]
pub struct DiscoveredPeers {
    pub peers: HashMap<PeerId, Option<Instant>>,
}

impl Discovery {
    pub async fn new(local_key: &Keypair) -> Self {
        // Setup socket
        let listen_socket = "0.0.0.0:9000".parse::<SocketAddr>().unwrap();

        // Generate ENR
        let enr_key: CombinedKey = key_from_libp2p(&local_key).unwrap();

        let local_enr = build_enr(&enr_key);

        // Setup default config
        let config = Discv5ConfigBuilder::new()
            .enable_packet_filter()
            .request_timeout(Duration::from_secs(1))
            .query_peer_timeout(Duration::from_secs(2))
            .query_timeout(Duration::from_secs(30))
            .request_retries(1)
            .enr_peer_update_min(10)
            .query_parallelism(5)
            .disable_report_discovered_peers()
            .ip_limit()
            .incoming_bucket_limit(8)
            .ping_interval(Duration::from_secs(300))
            .build();

        // Create discv5 instance
        let mut discv5 = Discv5::new(local_enr.clone(), enr_key, config).unwrap();

        // Add bootnode
        let ef_bootnode_enr = Enr::from_str(BOOTNODE).unwrap();
        discv5.add_enr(ef_bootnode_enr).expect("bootnode error");

        // Start the discv5 service
        discv5.start(listen_socket).await.unwrap();

        // Obtain an event stream
        let event_stream = EventStream::Awaiting(Box::pin(discv5.event_stream()));

        return Self {
            discv5,
            _enr: local_enr,
            event_stream,
            multiaddr_map: HashMap::new(),
            peers_future: FuturesUnordered::new(),
            started: false,
        };
    }

    fn find_peers(&mut self) {
        let fork_digest = self._enr.fork_id().unwrap().fork_digest;

        let predicate: Box<dyn Fn(&Enr) -> bool + Send> = Box::new(move |enr: &Enr| {
            enr.fork_id().map(|e| e.fork_digest) == Ok(fork_digest) && enr.tcp4().is_some()
        });

        let target = NodeId::random();

        let peers_enr = self.discv5.find_node_predicate(target, predicate, 16);

        self.peers_future.push(Box::pin(peers_enr));
    }

    fn get_peers(&mut self, cx: &mut Context) -> Option<DiscoveredPeers> {
        while let Poll::Ready(Some(res)) = self.peers_future.poll_next_unpin(cx) {
            if res.is_ok() {
                self.peers_future = FuturesUnordered::new();

                let mut peers: HashMap<PeerId, Option<Instant>> = HashMap::new();

                for peer_enr in res.unwrap() {
                    let peer_id = peer_enr.clone().as_peer_id();

                    if peer_enr.ip4().is_some() && peer_enr.tcp4().is_some() {
                        let mut multiaddr: Multiaddr = peer_enr.ip4().unwrap().into();

                        multiaddr.push(Protocol::Tcp(peer_enr.tcp4().unwrap()));

                        self.multiaddr_map.insert(peer_id, multiaddr);
                    }

                    peers.insert(peer_id, None);
                }

                println!("Found {} peers", peers.len());
                return Some(DiscoveredPeers { peers });
            }
        }

        None
    }
}

enum EventStream {
    Present(mpsc::Receiver<Discv5Event>),
    InActive,
    Awaiting(
        Pin<
            Box<
                dyn Future<Output = Result<mpsc::Receiver<Discv5Event>, discv5::Discv5Error>>
                    + Send,
            >,
        >,
    ),
}

impl NetworkBehaviour for Discovery {
    type ConnectionHandler = libp2p::swarm::dummy::ConnectionHandler;
    type OutEvent = DiscoveredPeers;

    fn new_handler(&mut self) -> Self::ConnectionHandler {
        libp2p::swarm::dummy::ConnectionHandler {}
    }

    fn addresses_of_peer(&mut self, peer_id: &PeerId) -> Vec<Multiaddr> {
        let mut peer_address: Vec<Multiaddr> = Vec::new();

        if let Some(address) = self.multiaddr_map.get(peer_id) {
            peer_address.push(address.clone());
        }

        return peer_address;
    }

    // Main execution loop to drive the behaviour
    fn poll(
        &mut self,
        cx: &mut Context,
        _: &mut impl PollParameters,
    ) -> Poll<NetworkBehaviourAction<Self::OutEvent, Self::ConnectionHandler>> {
        // println!("Discovery polled : {}", self.poll_count);
        self.poll_count += 1;
        if self.poll_count % 100 == 1 {
            self.started = true;
            println!("Finding Peers");
            self.find_peers();

            return Poll::Pending;
        }

        if let Some(dp) = self.get_peers(cx) {
            return Poll::Ready(NetworkBehaviourAction::GenerateEvent(dp));
        };

        // Process the discovery server event stream
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
                        Discv5Event::SessionEstablished(_enr, _) => {
                            // println!("Session Established: {:?}", enr);
                        }
                        _ => (),
                    }
                }
            }
        }
        Poll::Pending
    }
}

// Get CombinedKey from Secp256k1 libp2p Keypair
pub fn key_from_libp2p(key: &libp2p::core::identity::Keypair) -> Result<CombinedKey, &'static str> {
    match key {
        Keypair::Secp256k1(key) => {
            let secret = discv5::enr::k256::ecdsa::SigningKey::from_bytes(&key.secret().to_bytes())
                .expect("libp2p key must be valid");
            Ok(CombinedKey::Secp256k1(secret))
        }
        _ => Err("pair not supported"),
    }
}
