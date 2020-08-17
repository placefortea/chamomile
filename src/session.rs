use async_std::{
    io,
    io::BufReader,
    prelude::*,
    sync::{channel, Arc, Receiver, RwLock, Sender},
    task,
};
use futures::{select, FutureExt};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::Duration;

use crate::hole_punching::{nat, Hole, DHT};
use crate::keys::{KeyType, Keypair, SessionKey};
use crate::message::ReceiveMessage;
use crate::peer::{Peer, PeerId};
use crate::peer_list::PeerList;
use crate::primitives::MAX_MESSAGE_CAPACITY;
use crate::transports::{EndpointSendMessage, EndpointStreamMessage};

/// server send to session message in channel.
pub(crate) enum SessionSendMessage {
    /// when peer join ok, will send info to session, bool is stabled.
    Ok(Vec<u8>, bool),
    /// when peer join failure or close by self.
    Close,
    /// send bytes to session what want to send to peer..
    Bytes(PeerId, PeerId, Vec<u8>),
}

/// server receive from session message in channel.
pub(crate) enum SessionReceiveMessage {
    /// when connection is ok, will send info to server.
    Connected(PeerId, Sender<SessionSendMessage>, Peer, Vec<u8>, bool),
    /// when connection is closed.
    Close(PeerId),
    /// start try to connect this peer. from DHT help.
    Connect(SocketAddr),
}

/// new a channel for receive message from session.
pub(crate) fn new_session_receive_channel() -> (
    Sender<SessionReceiveMessage>,
    Receiver<SessionReceiveMessage>,
) {
    channel(MAX_MESSAGE_CAPACITY)
}

/// new a channel for send message to session.
pub(crate) fn new_session_send_channel(
) -> (Sender<SessionSendMessage>, Receiver<SessionSendMessage>) {
    channel(MAX_MESSAGE_CAPACITY)
}

/// start a session to handle incoming.
pub(crate) fn start(
    remote_addr: SocketAddr,
    endpoint_recv: Receiver<EndpointStreamMessage>,
    endpoint_send: Sender<EndpointStreamMessage>,
    server_send: Sender<SessionReceiveMessage>,
    out_sender: Sender<ReceiveMessage>,
    key: Arc<Keypair>,
    peer: Arc<Peer>,
    peer_list: Arc<RwLock<PeerList>>,
    mut is_ok: bool,
) {
    task::spawn(async move {
        // timeout 10s to read peer_id & public_key
        let result: io::Result<Option<RemotePublic>> = io::timeout(Duration::from_secs(5), async {
            while let Ok(msg) = endpoint_recv.recv().await {
                let remote_peer_key = match msg {
                    EndpointStreamMessage::Bytes(bytes) => {
                        RemotePublic::from_bytes(key.key, bytes).ok()
                    }
                    _ => None,
                };
                return Ok(remote_peer_key);
            }

            Ok(None)
        })
        .await;

        if result.is_err() {
            debug!("Debug: Session timeout");
            endpoint_send.send(EndpointStreamMessage::Close).await;
            drop(endpoint_recv);
            drop(endpoint_send);
            return;
        }
        let result = result.unwrap();
        if result.is_none() {
            debug!("Debug: Session invalid pk");
            endpoint_send.send(EndpointStreamMessage::Close).await;
            drop(endpoint_recv);
            drop(endpoint_send);
            return;
        }
        let RemotePublic(remote_peer_key, remote_local_addr, remote_join_data) = result.unwrap();
        let remote_peer_id = remote_peer_key.peer_id();
        let mut session_key: SessionKey = key.key.session_key(&key, &remote_peer_key);

        debug!("Debug: Session connected: {}", remote_peer_id.short_show());
        let (sender, receiver) = new_session_send_channel();
        let remote_transport = nat(remote_addr, remote_local_addr);
        debug!("Debug: NAT addr: {}", remote_transport.addr());
        server_send
            .send(SessionReceiveMessage::Connected(
                remote_peer_id,
                sender,
                remote_transport,
                remote_join_data,
                is_ok,
            ))
            .await;

        let mut buffers: Vec<(PeerId, Vec<u8>)> = vec![];
        let mut receiver_buffers: Vec<Vec<u8>> = vec![];
        let my_peer_id = peer.id().clone();

        loop {
            select! {
                msg = endpoint_recv.recv().fuse() => match msg {
                    Ok(msg) => {
                        if !is_ok {
                            continue;
                        }

                        match msg {
                            EndpointStreamMessage::Bytes(bytes) => {
                                match SessionType::from_bytes(bytes) {
                                    Ok(t) => match t {
                                        SessionType::Key(bytes) => {
                                            if !session_key.is_ok() {
                                                if !session_key.in_bytes(bytes) {
                                                    server_send
                                                        .send(
                                                            SessionReceiveMessage::Close(
                                                                remote_peer_id)).await;
                                                    endpoint_send.send(
                                                        EndpointStreamMessage::Close).await;
                                                    break;
                                                }

                                                endpoint_send
                                                    .send(EndpointStreamMessage::Bytes(
                                                        SessionType::Key(session_key.out_bytes())
                                                            .to_bytes()
                                                    ))
                                                    .await;
                                            }

                                            while !buffers.is_empty() {
                                                let (peer_id, bytes) = buffers.pop().unwrap();
                                                let e_data = session_key.encrypt(bytes);
                                                let data = SessionType::Data(
                                                    my_peer_id,
                                                    peer_id,
                                                    e_data
                                                ).to_bytes();
                                                endpoint_send
                                                    .send(EndpointStreamMessage::Bytes(data))
                                                    .await;
                                            }

                                            while !receiver_buffers.is_empty() {
                                                let e_data = receiver_buffers.pop().unwrap();
                                                let d_data = session_key.decrypt(e_data);
                                                if d_data.is_ok() {
                                                    out_sender
                                                        .send(ReceiveMessage::Data(
                                                            remote_peer_id,
                                                            d_data.unwrap()
                                                        )).await;
                                                }
                                            }

                                        }
                                        SessionType::Data(from, to, e_data) => {
                                            if !session_key.is_ok() {
                                                continue;
                                            }

                                            let d_data = session_key.decrypt(e_data);
                                            if d_data.is_ok() {
                                                if to == my_peer_id {
                                                    //debug!("DEBUG: Directly: from: {} to: {}, me: {}, remote: {}", from.short_show(), to.short_show(), my_peer_id.short_show(), remote_peer_id.short_show());
                                                    out_sender
                                                        .send(ReceiveMessage::Data(
                                                            from,
                                                            d_data.unwrap()
                                                        )).await;
                                                } else {
                                                    //debug!("DEBUG: Relay: from: {} to: {}, me: {}, remote: {}", from.short_show(), to.short_show(),  my_peer_id.short_show(), remote_peer_id.short_show());
                                                    // Relay
                                                    let peer_list_lock = peer_list.read().await;
                                                    let sender = peer_list_lock.get(&to);
                                                    if sender.is_some() {
                                                        let sender = sender.unwrap();
                                                        sender.send(
                                                            SessionSendMessage::Bytes(
                                                                from,
                                                                to,
                                                                d_data.unwrap()
                                                            )).await;
                                                    }
                                                }
                                            }
                                        }
                                        SessionType::DHT(peers, _sign) => {
                                            let DHT(peers) = peers;
                                            // TODO DHT Helper
                                            // remote_peer_key.verify()

                                            for p in peers {
                                                if p.id() != peer.id()
                                                    && peer_list.read().await
                                                    .get_it(p.id())
                                                    .is_none() {
                                                        server_send.send(
                                                            SessionReceiveMessage::Connect(
                                                                *p.addr(),
                                                            )).await;
                                                }
                                            }

                                        }
                                        SessionType::Ping => {
                                            endpoint_send
                                                .send(EndpointStreamMessage::Bytes(
                                                    SessionType::Pong.to_bytes()
                                                ))
                                                .await;
                                        }
                                        SessionType::Pong => {
                                            // TODO Heartbeat Ping/Pong
                                        },
                                        _ => {
                                            //TODO Hole
                                        }
                                    }
                                    Err(e) => {
                                        debug!("Debug: Error Serialize SessionType {:?}", e)
                                    },
                                }
                            },
                            EndpointStreamMessage::Close => {
                                server_send
                                    .send(SessionReceiveMessage::Close(remote_peer_id))
                                    .await;
                                break;
                            }
                            _ => break,
                        }
                    },
                    Err(_) => break,
                },
                out_msg = receiver.recv().fuse() => match out_msg {
                    Ok(msg) => {
                        match msg {
                            SessionSendMessage::Bytes(from, to, bytes) => {
                                if session_key.is_ok() {
                                    let e_data = session_key.encrypt(bytes);
                                    let data = SessionType::Data(from, to, e_data).to_bytes();
                                    endpoint_send
                                        .send(EndpointStreamMessage::Bytes(data))
                                        .await;
                                } else {
                                    buffers.push((to, bytes));
                                }
                            },
                            SessionSendMessage::Ok(data, _is_stable) => {
                                is_ok = true;

                                endpoint_send
                                    .send(EndpointStreamMessage::Bytes(
                                        RemotePublic(
                                            key.public(),
                                            *peer.clone(),
                                            data
                                        ).to_bytes()
                                    )).await;

                                endpoint_send
                                    .send(EndpointStreamMessage::Bytes(
                                        SessionType::Key(session_key.out_bytes()).to_bytes()
                                    ))
                                    .await;

                                let sign = vec![]; // TODO
                                let peers = peer_list.read().await.get_dht_help(&remote_peer_id);
                                endpoint_send
                                    .send(EndpointStreamMessage::Bytes(
                                        SessionType::DHT(DHT(peers), sign).to_bytes()
                                    ))
                                    .await;
                            },
                            SessionSendMessage::Close => {
                                endpoint_send.send(EndpointStreamMessage::Close).await;
                                break;
                            }
                        }
                    },
                    Err(_) => break,
                }
            }
        }
    });
}

#[derive(Deserialize, Serialize)]
enum SessionType {
    Key(Vec<u8>),
    Data(PeerId, PeerId, Vec<u8>),
    DHT(DHT, Vec<u8>),
    Hole(Hole),
    HoleConnect,
    Ping,
    Pong,
}

impl SessionType {
    fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap()
    }

    fn from_bytes(bytes: Vec<u8>) -> Result<Self, ()> {
        bincode::deserialize(&bytes[..]).map_err(|_e| ())
    }
}

/// Rtemote Public Info, include local transport and public key bytes.
#[derive(Deserialize, Serialize)]
pub struct RemotePublic(pub Keypair, pub Peer, pub Vec<u8>);

impl RemotePublic {
    pub fn from_bytes(key: KeyType, bytes: Vec<u8>) -> Result<Self, ()> {
        bincode::deserialize(&bytes).map_err(|_e| ())
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap()
    }
}
