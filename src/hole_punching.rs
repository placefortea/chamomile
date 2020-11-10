use smol::io::Result;
use std::net::SocketAddr;

use chamomile_types::types::{PeerId, TransportType};

use super::peer::{Peer, PEER_LENGTH};
use super::peer_list::PeerList;

pub(crate) enum Hole {
    StunOne,
    StunTwo,
    Help,
}

pub(crate) struct DHT(pub Vec<Peer>);

impl Hole {
    pub(crate) fn from_byte(byte: u8) -> std::result::Result<Self, ()> {
        match byte {
            0u8 => Ok(Hole::Help),
            1u8 => Ok(Hole::StunOne),
            2u8 => Ok(Hole::StunTwo),
            _ => Err(()),
        }
    }

    pub(crate) fn to_byte(&self) -> u8 {
        match self {
            Hole::Help => 0u8,
            Hole::StunOne => 1u8,
            Hole::StunTwo => 2u8,
        }
    }
}

impl DHT {
    pub(crate) fn from_bytes(bytes: &[u8]) -> std::result::Result<Self, ()> {
        if bytes.len() < 4 {
            return Err(());
        }
        let mut len_bytes = [0u8; 4];
        len_bytes.copy_from_slice(&bytes[0..4]);
        let len = u32::from_le_bytes(len_bytes) as usize;
        let raw_bytes = &bytes[4..];
        if raw_bytes.len() < len * PEER_LENGTH {
            return Err(());
        }
        let mut peers = vec![];
        for i in 0..len {
            peers.push(Peer::from_bytes(
                &raw_bytes[i * PEER_LENGTH..(i + 1) * PEER_LENGTH],
            )?);
        }
        Ok(Self(peers))
    }

    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = vec![];
        bytes.extend(&(self.0.len() as u32).to_le_bytes());
        for peer in &self.0 {
            bytes.append(&mut peer.to_bytes());
        }
        bytes
    }
}

pub fn nat(mut remote_addr: SocketAddr, mut local: Peer) -> Peer {
    match local.transport() {
        TransportType::TCP => {
            remote_addr.set_port(local.addr().port()); // TODO TCP hole punching
        }
        _ => {}
    }

    local.set_addr(remote_addr);
    local.set_is_pub(remote_addr.port() == local.addr().port());
    local
}

pub(crate) async fn handle(_remote_peer: &PeerId, hole: Hole, _peers: &PeerList) -> Result<()> {
    match hole {
        Hole::StunOne => {
            // first test
        }
        Hole::StunTwo => {
            // secound test
        }
        Hole::Help => {
            // help hole
        }
    }

    Ok(())
}
