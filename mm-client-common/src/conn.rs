// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

const DEFAULT_PORT: u16 = 9599;
const MAX_QUIC_PACKET_SIZE: usize = 1350;

const SOCKET: mio::Token = mio::Token(0);
const WAKER: mio::Token = mio::Token(1);

const CONNECT_TIMEOUT: time::Duration = time::Duration::from_secs(5);

use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::Arc,
    time,
};

use futures::channel::oneshot;
use tracing::{debug, error, trace, warn};

use mm_protocol as protocol;

#[derive(Debug, Clone, thiserror::Error)]
pub enum ConnError {
    #[error("invalid address: {0}")]
    InvalidAddress(String),
    #[error("unexpected OS error: {0}")]
    Unknown(#[from] Arc<std::io::Error>),
    #[error("QUIC error")]
    QuicError(#[from] quiche::Error),
    #[error("connection timeout")]
    Timeout,
    #[error("connection closed due to inactivity")]
    Idle,
    #[error("closed by peer (is_app={}, code={})", .0.is_app, .0.error_code)]
    PeerError(quiche::ConnectionError),
    #[error("recv or send queue is full")]
    QueueFull,
    #[error("protocol error")]
    ProtocolError(#[from] protocol::ProtocolError),
}

// In order to let ConnError implement Clone, we need to wrap io::Error in Arc;
// but then we lose From<io::Error>, which breaks the ? operator.
impl From<std::io::Error> for ConnError {
    fn from(e: std::io::Error) -> Self {
        Self::Unknown(Arc::new(e))
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ConnEvent {
    StreamMessage(u64, protocol::MessageType),
    Datagram(protocol::MessageType),
    StreamClosed(u64),
}

pub(crate) struct OutgoingMessage {
    pub(crate) sid: u64,
    pub(crate) msg: protocol::MessageType,
    pub(crate) fin: bool,
}

pub(crate) struct Conn {
    scratch: bytes::BytesMut,
    socket: mio::net::UdpSocket,
    local_addr: SocketAddr,
    poll: mio::Poll,
    waker: Arc<mio::Waker>,
    conn: quiche::Connection,
    partial_reads: HashMap<u64, bytes::BytesMut>,
    open_streams: HashSet<u64>,

    shutdown: oneshot::Receiver<()>,
    shutting_down: bool,

    incoming: flume::Sender<ConnEvent>,
    outgoing: flume::Receiver<OutgoingMessage>,

    ready: Option<oneshot::Sender<Result<(), ConnError>>>,
}

impl Conn {
    pub fn new(
        addr: &str,
        incoming: flume::Sender<ConnEvent>,
        outgoing: flume::Receiver<OutgoingMessage>,
        ready: oneshot::Sender<Result<(), ConnError>>,
        shutdown: oneshot::Receiver<()>,
    ) -> Result<Self, ConnError> {
        let (hostname, server_addr) = resolve_server(addr)?;
        let bind_addr = match server_addr {
            std::net::SocketAddr::V4(_) => "0.0.0.0:0",
            std::net::SocketAddr::V6(_) => "[::]:0",
        };

        let mut socket = mio::net::UdpSocket::bind(bind_addr.parse().unwrap())?;

        let mut config = quiche::Config::new(quiche::PROTOCOL_VERSION)?;

        if !ip_rfc::global(&server_addr.ip()) {
            warn!("skipping TLS verification for private server address");
            config.verify_peer(false);
        }

        config.set_application_protos(&[b"mm00"])?;

        config.set_max_idle_timeout(60_000);
        config.set_max_recv_udp_payload_size(MAX_QUIC_PACKET_SIZE);
        config.set_max_send_udp_payload_size(MAX_QUIC_PACKET_SIZE);
        config.set_initial_max_data(65536);
        config.set_initial_max_stream_data_bidi_local(65536);
        config.set_initial_max_stream_data_bidi_remote(6536);
        config.set_initial_max_streams_bidi(100);
        config.set_initial_max_stream_data_uni(65536);
        config.set_initial_max_streams_uni(100);
        config.enable_dgram(true, 65536, 0);

        let initial_scid = gen_scid();
        let local_addr = socket.local_addr().unwrap();
        let conn = quiche::connect(
            Some(&hostname),
            &initial_scid,
            local_addr,
            server_addr,
            &mut config,
        )?;

        let scratch = bytes::BytesMut::with_capacity(65536);

        let poll = mio::Poll::new().unwrap();
        let waker = Arc::new(mio::Waker::new(poll.registry(), WAKER)?);

        poll.registry()
            .register(&mut socket, SOCKET, mio::Interest::READABLE)?;

        Ok(Self {
            scratch,
            socket,
            local_addr,
            poll,
            waker,
            conn,
            partial_reads: HashMap::new(),
            open_streams: HashSet::new(),

            shutdown,
            shutting_down: false,

            incoming,
            outgoing,

            ready: Some(ready),
        })
    }

    pub fn waker(&self) -> Arc<mio::Waker> {
        self.waker.clone()
    }

    pub fn run(&mut self) -> Result<(), ConnError> {
        let mut events = mio::Events::with_capacity(1024);
        let start = time::Instant::now();

        loop {
            const ONE_SECOND: time::Duration = time::Duration::from_secs(1);
            let timeout = self
                .conn
                .timeout()
                .map_or(ONE_SECOND, |d| d.min(ONE_SECOND));

            self.poll.poll(&mut events, Some(timeout))?;

            let now = time::Instant::now();
            if self.conn.timeout_instant().is_some_and(|t| now >= t) {
                self.conn.on_timeout();
            }

            if self.conn.is_closed() || self.conn.is_draining() {
                if self.conn.is_timed_out() {
                    return Err(ConnError::Idle);
                } else if self.conn.is_dgram_recv_queue_full() {
                    return Err(ConnError::QueueFull);
                } else if let Some(err) = self.conn.peer_error() {
                    return Err(ConnError::PeerError(err.clone()));
                } else if !self.shutting_down {
                    panic!("connection closed unexpectedly");
                } else {
                    return Ok(());
                }
            }

            if self.ready.is_some() {
                if self.conn.is_established() || self.conn.is_in_early_data() {
                    trace!("connection ready");
                    let _ = self.ready.take().unwrap().send(Ok(()));
                } else if start.elapsed() > CONNECT_TIMEOUT {
                    let _ = self.ready.take().unwrap().send(Err(ConnError::Timeout));
                }
            }

            if let Ok(Some(())) = self.shutdown.try_recv() {
                self.start_shutdown()?;
            }

            // if (now - self.stats_timer) > time::Duration::from_millis(200) {
            //     self.stats_timer = now;
            //     let stats = self.conn.path_stats().next().unwrap();
            //     STATS.set_rtt(stats.rtt);
            // }

            // Read incoming UDP packets and handle them.
            loop {
                // TODO: use recv_mmsg for a small efficiency boost.
                self.scratch.resize(MAX_QUIC_PACKET_SIZE, 0);
                let (len, from) = match self.socket.recv_from(&mut self.scratch) {
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        break;
                    }
                    v => v?,
                };

                self.conn.recv(
                    &mut self.scratch[..len],
                    quiche::RecvInfo {
                        from,
                        to: self.local_addr,
                    },
                )?;
            }

            if (self.conn.is_established() || self.conn.is_in_early_data()) && !self.shutting_down {
                // Demux incoming messages and datagrams.
                for sid in self.conn.readable() {
                    self.open_streams.insert(sid);
                    self.pump_stream(sid)?;
                }

                loop {
                    self.scratch.resize(protocol::MAX_MESSAGE_SIZE, 0);
                    match self.conn.dgram_recv(&mut self.scratch) {
                        Ok(len) => {
                            let (msg, msg_len) = match protocol::decode_message(&self.scratch) {
                                Ok(v) => v,
                                Err(protocol::ProtocolError::InvalidMessageType(t, _)) => {
                                    warn!(msg_type = t, "ignoring unknown message type");
                                    continue;
                                }
                                Err(e) => return Err(e.into()),
                            };

                            debug_assert_eq!(msg_len, len);
                            trace!(%msg, len, "received datagram");

                            match self.incoming.send(ConnEvent::Datagram(msg)) {
                                Ok(()) => {}
                                Err(_) => {
                                    self.start_shutdown()?;
                                    break;
                                }
                            }
                        }
                        Err(quiche::Error::Done) => break,
                        Err(e) => {
                            error!("QUIC recv error: {:#}", e);
                            break;
                        }
                    }
                }

                // Enqueue outgoing messages.
                loop {
                    match self.outgoing.try_recv() {
                        Ok(OutgoingMessage { sid, msg, fin }) => {
                            if matches!(
                                self.conn.stream_capacity(sid),
                                Err(quiche::Error::InvalidState)
                                    | Err(quiche::Error::StreamStopped(_))
                            ) {
                                debug!(sid, %msg, "dropping outgoing message for finished stream");
                                continue;
                            }

                            self.open_streams.insert(sid);
                            self.send_message(sid, msg, fin)?;
                        }
                        Err(flume::TryRecvError::Empty) => {
                            break;
                        }
                        Err(flume::TryRecvError::Disconnected) => {
                            self.start_shutdown()?;
                            break;
                        }
                    }
                }

                // Garbage collect closed streams.
                let mut closed = Vec::new();
                self.open_streams.retain(|sid| {
                    if self.conn.stream_finished(*sid) {
                        trace!(sid, "stream finished");
                        closed.push(*sid);
                        false
                    } else {
                        true
                    }
                });

                for sid in closed {
                    match self.incoming.send(ConnEvent::StreamClosed(sid)) {
                        Ok(()) => {}
                        Err(_) => {
                            self.start_shutdown()?;
                            break;
                        }
                    }
                }
            }

            // Write out UDP packets.
            loop {
                self.scratch.resize(MAX_QUIC_PACKET_SIZE, 0);
                let (len, send_info) = match self.conn.send(&mut self.scratch) {
                    Ok(v) => v,
                    Err(quiche::Error::Done) => break,
                    Err(e) => {
                        error!("QUIC send error: {:#}", e);
                        break;
                    }
                };

                // TODO implement pacing with SO_TXTIME. (We can do
                // sendmmsg at the same time).
                self.socket.send_to(&self.scratch[..len], send_info.to)?;
            }
        }
    }

    fn pump_stream(&mut self, sid: u64) -> Result<bool, ConnError> {
        use bytes::Buf;

        self.scratch.truncate(0);
        if let Some(partial) = self.partial_reads.remove(&sid) {
            self.scratch.unsplit(partial);
        }

        let mut off = self.scratch.len();
        let mut stream_fin = false;
        loop {
            self.scratch.resize(off + protocol::MAX_MESSAGE_SIZE, 0);
            match self.conn.stream_recv(sid, &mut self.scratch[off..]) {
                Ok((len, fin)) => {
                    off += len;

                    if fin {
                        stream_fin = true;
                        break;
                    }
                }
                Err(quiche::Error::Done) => break,
                Err(e) => return Err(e.into()),
            }
        }

        // Read messages (there may be multiple).
        self.scratch.truncate(off);
        let mut buf = self.scratch.split();
        while !buf.is_empty() {
            let (msg, len) = match protocol::decode_message(&buf) {
                Ok(v) => v,
                Err(protocol::ProtocolError::ShortBuffer(n)) => {
                    debug!("partial message on stream {}, need {} bytes", sid, n);
                    self.partial_reads.insert(sid, buf);
                    break;
                }
                Err(e) => {
                    error!("protocol error: {:#}", e);
                    break;
                }
            };

            trace!(
                sid,
                %msg,
                len,
                fin = stream_fin,
                "received msg",
            );

            buf.advance(len);
            match self.incoming.send(ConnEvent::StreamMessage(sid, msg)) {
                Ok(()) => {}
                Err(_) => {
                    self.start_shutdown()?;
                    break;
                }
            }
        }

        Ok(stream_fin)
    }

    fn send_message(
        &mut self,
        sid: u64,
        msg: protocol::MessageType,
        fin: bool,
    ) -> Result<(), ConnError> {
        self.scratch.resize(protocol::MAX_MESSAGE_SIZE, 0);
        let len = protocol::encode_message(&msg, &mut self.scratch)?;

        trace!(sid, %msg, fin, "sending message");
        self.conn.stream_send(sid, &self.scratch[..len], fin)?;

        Ok(())
    }

    fn start_shutdown(&mut self) -> Result<(), ConnError> {
        match self.conn.close(true, 0x00, b"") {
            Ok(()) | Err(quiche::Error::Done) => (),
            Err(e) => return Err(e.into()),
        }
        self.shutting_down = true;
        Ok(())
    }
}

fn gen_scid() -> quiche::ConnectionId<'static> {
    use ring::rand::SecureRandom;

    let mut scid = vec![0; quiche::MAX_CONN_ID_LEN];

    ring::rand::SystemRandom::new().fill(&mut scid[..]).unwrap();
    quiche::ConnectionId::from_vec(scid)
}

fn resolve_server(hostport: &str) -> Result<(String, SocketAddr), ConnError> {
    use std::net::ToSocketAddrs;

    let parts = hostport.splitn(2, ':').collect::<Vec<_>>();
    let (host, port) = match parts[..] {
        [host] => {
            debug!("assuming default port {}", DEFAULT_PORT);
            (host, DEFAULT_PORT)
        }
        [host, port] => {
            if let Ok(port) = port.parse() {
                (host, port)
            } else {
                return Err(ConnError::InvalidAddress(hostport.to_string()));
            }
        }
        _ => return Err(ConnError::InvalidAddress(hostport.to_string())),
    };

    let addr = (host, port)
        .to_socket_addrs()
        .map_err(|_| ConnError::InvalidAddress(hostport.to_string()))?
        .next()
        .unwrap();

    Ok((host.to_string(), addr))
}
