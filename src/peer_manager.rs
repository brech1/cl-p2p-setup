use libp2p::{
    swarm::{
        behaviour::{ConnectionClosed, ConnectionEstablished, DialFailure, FromSwarm},
        dummy::ConnectionHandler,
        NetworkBehaviour, NetworkBehaviourAction, PollParameters,
    },
    PeerId,
};
use std::collections::{HashMap, HashSet};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

pub struct PeerManager {
    connected_peers: HashSet<PeerId>,
    dialing_peers: HashSet<PeerId>,
    new_peers: HashSet<PeerId>,
    peer_data: HashMap<PeerId, PeerData>,
    target_peer_number: u32,
    peers_to_discover: u32,
    /// The heartbeat interval to perform routine maintenance.
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

#[derive(Debug)]
pub struct PeerData {
    connection_history: Vec<ConnectionData>,
    connection_status: ConnectionStatus,
    average_connection_duration: Option<usize>,
}

impl PeerData {
    pub fn new() -> Self {
        Self {
            connection_history: Vec::new(),
            connection_status: ConnectionStatus::New,
            average_connection_duration: None,
        }
    }
}

const MIN_AVERAGE_CONNECTION_DURATION: usize = 5000;

#[derive(Debug)]
pub struct ConnectionData {
    established_timestamp: Option<Instant>,
    failure_timestamp: Option<Instant>,
    disconnect_timestamp: Option<Instant>,
}

#[derive(Debug)]
pub enum ConnectionStatus {
    New,
    Connecting,
    Connected,
    Disconnected,
    Failed,
}

const HEARTBEAT_INTERVAL: u64 = 1;

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
            let missing_peers =
                self.target_peer_number - self.connected_and_dialing_peers().len() as u32;
            if missing_peers > 0 {
                let peers_to_dial = self.get_peers_to_dial(missing_peers);
                self.peers_to_discover = missing_peers - peers_to_dial.len() as u32;

                if peers_to_dial.len() > 0 {
                    return Poll::Ready(NetworkBehaviourAction::GenerateEvent(
                        PeerManagerEvent::DialPeers(peers_to_dial),
                    ));
                }
            }
            return Poll::Pending;
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
                endpoint,
                other_established,
                ..
            }) => self.on_connection_established(peer_id),
            FromSwarm::ConnectionClosed(ConnectionClosed {
                peer_id,
                remaining_established,
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
}
impl PeerManager {
    pub fn new(target_peer_number: u32) -> Self {
        // Set up the peer manager heartbeat interval
        let heartbeat = tokio::time::interval(tokio::time::Duration::from_secs(HEARTBEAT_INTERVAL));
        Self {
            new_peers: HashSet::new(),
            connected_peers: HashSet::new(),
            dialing_peers: HashSet::new(),
            peer_data: HashMap::new(),
            peers_to_discover: 0,
            target_peer_number,
            heartbeat,
            waiting_for_peer_discovery: false,
        }
    }

    fn get_peers_to_dial(&mut self, missing_peers: u32) -> Vec<PeerId> {
        let mut peers_to_dial = Vec::new();
        peers_to_dial.append(&mut self.get_best_peers_for_redial(missing_peers));

        let new_peers_to_dial = missing_peers - peers_to_dial.len() as u32;
        if new_peers_to_dial > 0 {
            peers_to_dial.append(&mut self.get_new_peers_for_dialing(new_peers_to_dial));
        }

        for peer in peers_to_dial.iter() {
            self.dialing_peers.insert(*peer);
        }

        peers_to_dial
    }

    fn get_new_peers_for_dialing(&mut self, missing_peers: u32) -> Vec<PeerId> {
        let mut peers_to_dial = Vec::new();
        for peer_id in self.new_peers.clone().iter() {
            if peers_to_dial.len() == missing_peers as usize {
                break;
            }
            peers_to_dial.push(peer_id.clone());
            self.peer_connecting(peer_id.clone());
        }
        peers_to_dial
    }

    pub fn add_peers(&mut self, peer_ids: Vec<PeerId>) {
        for peer_id in peer_ids {
            self.peer_data.insert(peer_id, PeerData::new());
            self.new_peers.insert(peer_id);
        }
        self.waiting_for_peer_discovery = false;
    }

    fn peer_connecting(&mut self, peer_id: PeerId) {
        let peer_data = self.peer_data.get_mut(&peer_id).unwrap();
        peer_data.connection_status = ConnectionStatus::Connecting;
        peer_data.connection_history.push(ConnectionData {
            established_timestamp: None,
            failure_timestamp: None,
            disconnect_timestamp: None,
        });
        self.new_peers.remove(&peer_id);
    }

    fn on_connection_established(&mut self, peer_id: PeerId) {
        let peer_data = self.peer_data.get_mut(&peer_id).unwrap();
        peer_data.connection_status = ConnectionStatus::Connected;
        peer_data.connection_history.push(ConnectionData {
            established_timestamp: Some(Instant::now()),
            failure_timestamp: None,
            disconnect_timestamp: None,
        });
        self.connected_peers.insert(peer_id);
        self.dialing_peers.remove(&peer_id);
    }

    fn on_connection_closed(&mut self, peer_id: PeerId) {
        let peer_data = self.peer_data.get_mut(&peer_id).unwrap();
        peer_data.connection_status = ConnectionStatus::Disconnected;
        peer_data
            .connection_history
            .last_mut()
            .unwrap()
            .disconnect_timestamp = Some(Instant::now());
        PeerManager::update_average_connection_duration(peer_data);
        self.connected_peers.remove(&peer_id);
    }

    fn on_dial_failure(&mut self, peer_id: Option<PeerId>) {
        if let Some(peer_id) = peer_id {
            let peer_data = self.peer_data.get_mut(&peer_id).unwrap();
            peer_data.connection_status = ConnectionStatus::Failed;
            peer_data.connection_history.push(ConnectionData {
                established_timestamp: None,
                failure_timestamp: Some(Instant::now()),
                disconnect_timestamp: None,
            });
            PeerManager::update_average_connection_duration(peer_data);
            self.dialing_peers.remove(&peer_id);
        }
    }

    fn update_average_connection_duration(peer_data: &mut PeerData) {
        println!("Updating average connection duration: {:?}", peer_data);
        let num_connection_attempts = peer_data.connection_history.len();
        let new_duration = match peer_data.connection_status {
            ConnectionStatus::Failed => Duration::from_secs(0),
            ConnectionStatus::Disconnected => {
                let connection_data = peer_data.connection_history.last().unwrap();
                if let Some(disconnected_timestamp ) = connection_data.disconnect_timestamp {
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

        if let Some(old_average) = peer_data.average_connection_duration {
            peer_data.average_connection_duration = Some(
                (old_average * (num_connection_attempts - 1) + new_duration.as_millis() as usize)
                    / num_connection_attempts,
            );
        } else {
            peer_data.average_connection_duration = Some(new_duration.as_millis() as usize);
        }
    }

    fn connected_and_dialing_peers(&self) -> HashSet<PeerId> {
        let mut connected_and_dialing_peers = self.connected_peers.clone();
        connected_and_dialing_peers.extend(self.dialing_peers.clone());
        connected_and_dialing_peers
    }

    fn get_best_peers_for_redial(&self, num_peers: u32) -> Vec<PeerId> {
        // Get the best num_peers peers that we have previously connected to and that have been
        // connected on average for at least MINIMUM_AVERAGE_CONNECTION_DURATION
        let mut peer_data: Vec<(PeerId, &PeerData)> = self
            .peer_data
            .iter()
            .filter(|(_, peer_data)| match peer_data.connection_status {
                ConnectionStatus::Failed => true,
                ConnectionStatus::Disconnected => true,
                _ => false,
            })
            .filter(|(_, peer_data)| {
                peer_data.average_connection_duration.unwrap_or(0) > MIN_AVERAGE_CONNECTION_DURATION
            })
            .map(|(peer_id, peer_data)| (peer_id.clone(), peer_data))
            .collect();
        peer_data.sort_by(|(_, a), (_, b)| {
            let a_score = PeerManager::get_peer_score(a);
            let b_score = PeerManager::get_peer_score(b);
            a_score.cmp(&b_score)
        });
        peer_data
            .iter()
            .take(num_peers as usize)
            .map(|(peer_id, _)| *peer_id)
            .collect()
    }

    fn get_peer_score(peer_data: &PeerData) -> usize {
        if let Some(score) = peer_data.average_connection_duration {
            return score;
        }
        0
    }
}
