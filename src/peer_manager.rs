use libp2p::{
    identify::IdentifyInfo,
    swarm::{
        behaviour::{ConnectionClosed, ConnectionEstablished, DialFailure, FromSwarm},
        dummy::ConnectionHandler,
        NetworkBehaviour, NetworkBehaviourAction, PollParameters,
    },
    Multiaddr, PeerId,
};
use serde::{Deserialize, Serialize};

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::prelude::*;
use std::path::Path;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use log::{debug, info};

pub struct PeerManager {
    connected_peers: HashSet<PeerId>,
    dialling_peers: HashSet<PeerId>,
    new_peers: HashSet<PeerId>,
    peer_data: HashMap<PeerId, PeerData>,
    peer_identities: HashMap<PeerId, IdentifyInfo>,
    target_peer_number: u32,
    peers_to_discover: u32,
    /// The heartbeat interval to timeout dialling peers
    heartbeat: tokio::time::Interval,
    waiting_for_peer_discovery: bool,
}

#[derive(Debug)]
pub enum PeerManagerEvent {
    /// Request the behaviour to discover more peers and the amount of peers to discover.
    DiscoverPeers(u32),
    /// Request the swarm to dial the given peer ids
    DialPeers(Vec<PeerId>),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PeerData {
    pub connection_history: Vec<ConnectionData>,
    pub average_connection_duration: Option<usize>,
    pub multiaddr: Option<Multiaddr>,
}

impl PeerData {
    pub fn new(multiaddr: Option<Multiaddr>) -> Self {
        Self {
            connection_history: Vec::new(),
            average_connection_duration: None,
            multiaddr,
        }
    }
}

const MIN_AVERAGE_CONNECTION_DURATION: usize = 3000;

#[derive(Debug, Serialize, Deserialize)]
pub struct ConnectionData {
    #[serde(with = "serde_millis")]
    pub established_timestamp: Option<Instant>,
    #[serde(with = "serde_millis")]
    pub failure_timestamp: Option<Instant>,
    #[serde(with = "serde_millis")]
    pub disconnect_timestamp: Option<Instant>,
    #[serde(with = "serde_millis")]
    pub dial_timestamp: Instant,
    pub connection_status: ConnectionStatus,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub enum ConnectionStatus {
    Connecting,
    Connected,
    Disconnected,
    Failed,
    Timeout,
}

// Interval for which to check if we need to dial new peers
const HEARTBEAT_INTERVAL: u64 = 500;
// Consider connection attempt timed out if it takes longer than this duration (in ms)
const DIAL_TIMEOUT: u64 = 5000;

impl NetworkBehaviour for PeerManager {
    type ConnectionHandler = ConnectionHandler;

    type OutEvent = PeerManagerEvent;

    fn poll(
        &mut self,
        cx: &mut Context<'_>,
        _params: &mut impl PollParameters,
    ) -> Poll<NetworkBehaviourAction<Self::OutEvent, Self::ConnectionHandler>> {
        // perform the heartbeat when necessary
        if !self.waiting_for_peer_discovery && self.peers_to_discover > 0 {
            let ev = Poll::Ready(NetworkBehaviourAction::GenerateEvent(
                PeerManagerEvent::DiscoverPeers(self.peers_to_discover),
            ));
            self.peers_to_discover = 0;
            self.waiting_for_peer_discovery = true;
            return ev;
        }
        while self.heartbeat.poll_tick(cx).is_ready() {
            self.timeout_dialling_peers();
            let missing_peers =
                self.target_peer_number - self.connected_and_dialling_peers().len() as u32;
            if missing_peers > self.peers_to_discover / 4 {
                let peers_to_dial = self.get_peers_to_dial(missing_peers);
                self.peers_to_discover = missing_peers - peers_to_dial.len() as u32;

                if !peers_to_dial.is_empty() {
                    return Poll::Ready(NetworkBehaviourAction::GenerateEvent(
                        PeerManagerEvent::DialPeers(peers_to_dial),
                    ));
                }
            }
        }
        Poll::Pending
    }

    fn new_handler(&mut self) -> Self::ConnectionHandler {
        ConnectionHandler
    }

    fn on_swarm_event(&mut self, event: FromSwarm<Self::ConnectionHandler>) {
        match event {
            FromSwarm::ConnectionEstablished(ConnectionEstablished {
                peer_id,
                endpoint: _,
                other_established: _,
                ..
            }) => self.on_connection_established(peer_id),
            FromSwarm::ConnectionClosed(ConnectionClosed {
                peer_id,
                remaining_established: _,
                ..
            }) => self.on_connection_closed(peer_id),
            FromSwarm::DialFailure(DialFailure { peer_id, .. }) => self.on_dial_failure(peer_id),
            FromSwarm::AddressChange(_)
            | FromSwarm::ListenFailure(_)
            | FromSwarm::NewListener(_)
            | FromSwarm::NewListenAddr(_)
            | FromSwarm::ExpiredListenAddr(_)
            | FromSwarm::ListenerError(_)
            | FromSwarm::ListenerClosed(_)
            | FromSwarm::NewExternalAddr(_)
            | FromSwarm::ExpiredExternalAddr(_) => {
                // The rest of the events we ignore since they are handled in their associated
                // `SwarmEvent`
            }
        }
    }

    fn addresses_of_peer(&mut self, peer_id: &PeerId) -> Vec<Multiaddr> {
        let mut peer_address: Vec<Multiaddr> = Vec::new();

        if let Some(peer_data) = self.peer_data.get(peer_id) {
            if let Some(address) = &peer_data.multiaddr {
                peer_address.push(address.clone());
            }
        }

        peer_address
    }
}
impl PeerManager {
    pub fn new(target_peer_number: u32) -> Self {
        // Set up the peer manager heartbeat interval
        let heartbeat =
            tokio::time::interval(tokio::time::Duration::from_millis(HEARTBEAT_INTERVAL));
        Self {
            new_peers: HashSet::new(),
            connected_peers: HashSet::new(),
            dialling_peers: HashSet::new(),
            peer_data: HashMap::new(),
            peer_identities: HashMap::new(),
            peers_to_discover: 0,
            target_peer_number,
            heartbeat,
            waiting_for_peer_discovery: false,
        }
    }

    fn timeout_dialling_peers(&mut self) {
        let now = Instant::now();
        let mut timed_out_peers = Vec::new();
        for peer_id in self.dialling_peers.iter() {
            if let Some(peer_data) = self.peer_data.get_mut(peer_id) {
                if let Some(connection_data) = peer_data.connection_history.last_mut() {
                    if connection_data.connection_status == ConnectionStatus::Connecting
                        && now
                            .duration_since(connection_data.dial_timestamp)
                            .as_millis()
                            > DIAL_TIMEOUT.into()
                    {
                        connection_data.connection_status = ConnectionStatus::Timeout;
                        connection_data.failure_timestamp = Some(now);
                        timed_out_peers.push(*peer_id);
                    }
                } else {
                    timed_out_peers.push(*peer_id);
                }
            } else {
                timed_out_peers.push(*peer_id);
            }
        }
        for peer_id in timed_out_peers {
            self.dialling_peers.remove(&peer_id);
        }
    }

    fn get_peers_to_dial(&mut self, missing_peers: u32) -> Vec<PeerId> {
        let mut peers_to_dial = Vec::new();
        peers_to_dial.append(&mut self.get_best_peers_for_redial(missing_peers));

        let new_peers_to_dial = missing_peers - peers_to_dial.len() as u32;
        if new_peers_to_dial > 0 {
            peers_to_dial.append(&mut self.get_new_peers_for_dialling(new_peers_to_dial));
        }

        for peer in peers_to_dial.iter() {
            self.dialling_peers.insert(*peer);
            self.peer_connecting(*peer);
        }

        peers_to_dial
    }

    fn get_new_peers_for_dialling(&mut self, missing_peers: u32) -> Vec<PeerId> {
        let mut peers_to_dial = Vec::new();
        for peer_id in self.new_peers.clone().iter() {
            if peers_to_dial.len() == missing_peers as usize {
                break;
            }
            peers_to_dial.push(*peer_id);
        }
        peers_to_dial
    }

    pub fn add_peers(&mut self, peer_ids: HashMap<PeerId, Option<Multiaddr>>) {
        for (peer_id, multiaddr) in peer_ids.iter() {
            if self.peer_data.contains_key(peer_id) {
                continue;
            }
            let multiaddr = multiaddr.as_ref().cloned();
            self.peer_data.insert(*peer_id, PeerData::new(multiaddr));
            self.new_peers.insert(*peer_id);
        }
        self.waiting_for_peer_discovery = false;
    }

    fn peer_connecting(&mut self, peer_id: PeerId) {
        let peer_data = self.peer_data.get_mut(&peer_id).unwrap();
        peer_data.connection_history.push(ConnectionData {
            established_timestamp: None,
            failure_timestamp: None,
            disconnect_timestamp: None,
            connection_status: ConnectionStatus::Connecting,
            dial_timestamp: Instant::now(),
        });
        self.new_peers.remove(&peer_id);
    }

    fn on_connection_established(&mut self, peer_id: PeerId) {
        info!("Connection established with peer {}", peer_id);
        let peer_data = self.peer_data.get_mut(&peer_id).unwrap();
        let mut connection_data = peer_data
            .connection_history
            .last_mut()
            .expect("Missing connectio_data entry for established peer");
        connection_data.established_timestamp = Some(Instant::now());
        connection_data.connection_status = ConnectionStatus::Connected;

        self.connected_peers.insert(peer_id);
        self.dialling_peers.remove(&peer_id);
    }

    fn on_connection_closed(&mut self, peer_id: PeerId) {
        info!("Connection closed with peer {}", peer_id);
        let peer_data = self.peer_data.get_mut(&peer_id).unwrap();
        let mut connection_data = peer_data
            .connection_history
            .last_mut()
            .expect("Missing connectio_data entry for established peer");
        connection_data.connection_status = ConnectionStatus::Disconnected;
        connection_data.disconnect_timestamp = Some(Instant::now());
        PeerManager::update_average_connection_duration(peer_data);
        self.connected_peers.remove(&peer_id);
    }

    fn on_dial_failure(&mut self, peer_id: Option<PeerId>) {
        info!("Dialling failed for peer: {:?}", peer_id);
        if let Some(peer_id) = peer_id {
            let peer_data = self.peer_data.get_mut(&peer_id).unwrap();
            let mut connection_data = peer_data
                .connection_history
                .last_mut()
                .expect("Missing connectio_data entry for established peer");
            connection_data.connection_status = ConnectionStatus::Failed;
            connection_data.failure_timestamp = Some(Instant::now());
            PeerManager::update_average_connection_duration(peer_data);
            self.dialling_peers.remove(&peer_id);
        }
    }

    fn update_average_connection_duration(peer_data: &mut PeerData) {
        let num_connection_attempts = peer_data.connection_history.len();
        let connection_data = peer_data
            .connection_history
            .last()
            .expect("Missing connection_data entry when updating average connection");
        let new_duration = match connection_data.connection_status {
            ConnectionStatus::Failed => Duration::from_secs(0),
            ConnectionStatus::Disconnected => {
                if let Some(disconnected_timestamp) = connection_data.disconnect_timestamp {
                    if let Some(established_timestamp) = connection_data.established_timestamp {
                        disconnected_timestamp - established_timestamp
                    } else {
                        Duration::from_secs(0)
                    }
                } else {
                    Duration::from_secs(0)
                }
            }
            _ => panic!(
                "Connection status should be either failed or disconnected when updating score"
            ),
        };

        info!(
            "Updating average connection duration withs status {:?} with new duration {:?}",
            connection_data.connection_status, new_duration
        );

        if let Some(old_average) = peer_data.average_connection_duration {
            peer_data.average_connection_duration = Some(
                (old_average * (num_connection_attempts - 1) + new_duration.as_millis() as usize)
                    / num_connection_attempts,
            );
        } else {
            peer_data.average_connection_duration = Some(new_duration.as_millis() as usize);
        }
    }

    fn connected_and_dialling_peers(&self) -> HashSet<PeerId> {
        let mut connected_and_dialling_peers = self.connected_peers.clone();
        info!("Connected peers: {:?}", connected_and_dialling_peers.len());
        connected_and_dialling_peers.extend(self.dialling_peers.clone());
        info!(
            "Connected and dialling peers: {:?}",
            connected_and_dialling_peers.len()
        );
        connected_and_dialling_peers
    }

    fn get_best_peers_for_redial(&self, num_peers: u32) -> Vec<PeerId> {
        // Get the best num_peers peers that we have previously connected to and that have been
        // connected on average for at least MINIMUM_AVERAGE_CONNECTION_DURATION
        let mut peer_data: Vec<(PeerId, &PeerData)> = self
            .peer_data
            .iter()
            .filter(|(_, peer_data)| match peer_data.connection_history.last() {
                Some(connection_data) => match connection_data.connection_status {
                    ConnectionStatus::Failed => true,
                    ConnectionStatus::Disconnected => true,
                    _ => false,
                },
                _ => false,
            })
            .filter(|(_, peer_data)| {
                peer_data.average_connection_duration.unwrap_or(0) > MIN_AVERAGE_CONNECTION_DURATION
            })
            .map(|(peer_id, peer_data)| (*peer_id, peer_data))
            .collect();
        peer_data.sort_by(|(_, a), (_, b)| {
            let a_score = PeerManager::get_peer_score(a);
            let b_score = PeerManager::get_peer_score(b);
            a_score.cmp(&b_score)
        });
        let best_peers = peer_data
            .iter()
            .take(num_peers as usize)
            .map(|(peer_id, _)| *peer_id)
            .collect::<Vec<_>>();
        info!("Found {:} peers for redialing", best_peers.len());
        debug!("Best peers for redial: {:?}", best_peers);
        best_peers
    }

    fn get_peer_score(peer_data: &PeerData) -> usize {
        if let Some(score) = peer_data.average_connection_duration {
            return score;
        }
        0
    }

    pub fn add_peer_identity(&mut self, peer_id: PeerId, identity: IdentifyInfo) {
        self.peer_identities.insert(peer_id, identity);
    }

    pub fn log_identities(&self) {
        info!("Peer identities: {:#?}", self.peer_identities);
    }

    pub fn log_metrics(&self) {
        let mut number_and_average_time_by_peer = self
            .peer_data
            .iter()
            .map(|(peer_id, peer_data)| {
                let num_connections = peer_data.connection_history.len();
                let average_connection_duration =
                    peer_data.average_connection_duration.unwrap_or(0);
                let connection_status = peer_data
                    .connection_history
                    .last()
                    .map(|connection_data| &connection_data.connection_status);
                (
                    peer_id,
                    num_connections,
                    Duration::from_millis(average_connection_duration as u64),
                    connection_status,
                )
            })
            .collect::<Vec<_>>();
        number_and_average_time_by_peer.sort_by(|(_, _, a, _), (_, _, b, _)| b.cmp(a));
        info!(
            "Number and average time by peer: {:#?}",
            number_and_average_time_by_peer
        );

        info!("Connected peers: {:#?}", self.connected_peers);
        info!("dialling Peers: {:#?}", self.dialling_peers);
    }

    pub fn save_peer_data(&mut self, filename: &str) {
        self.disconnect_all_peers();
        let mut file = File::create(filename).expect("Unable to create file");
        file.write_all(
            serde_json::to_string_pretty(&self.peer_data)
                .expect("Unable to serialize peer data")
                .as_bytes(),
        )
        .expect("Unable to write to file");
    }

    fn disconnect_all_peers(&mut self) {
        let peers_to_disconnect = self.connected_peers.iter().cloned().collect::<Vec<_>>();
        for peer_id in peers_to_disconnect {
            self.on_connection_closed(peer_id);
        }
    }

    pub fn load_peer_data(&mut self, filename: &str) {
        if Path::new(filename).is_file() {
            info!("Reading peer data from file");
            let mut file = File::open(filename).expect("Unable to open file");
            let mut contents = String::new();
            file.read_to_string(&mut contents)
                .expect("Unable to read file");
            self.peer_data =
                serde_json::from_str(&contents).expect("Unable to deserialize peer data");
            debug!("Successfully loaded peer_data: {:#?}", self.peer_data);
        } else {
            info!("No peer data file found");
        }
    }
}
