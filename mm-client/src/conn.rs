// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use std::{
    collections::{HashMap, HashSet},
    net::SocketAddr,
    sync::Arc,
    time,
};

use anyhow::{anyhow, bail, Context};
use crossbeam_channel as crossbeam;
use mm_protocol as protocol;
use tracing::{debug, error, instrument, trace, warn};

use crate::stats::STATS;

const DEFAULT_PORT: u16 = 9599;

#[derive(Debug, Clone)]
pub enum ConnEvent {
    StreamMessage(u64, protocol::MessageType),
    Datagram(protocol::MessageType),
    StreamClosed(u64),
    ConnectionClosed,
}

pub struct OutgoingMessage {
    pub sid: u64,
    pub msg: protocol::MessageType,
    pub fin: bool,
}

pub struct Conn {
    thread_handle: std::thread::JoinHandle<anyhow::Result<()>>,
    waker: Arc<mio::Waker>,
    pub incoming: crossbeam::Receiver<ConnEvent>,
    pub outgoing: crossbeam::Sender<OutgoingMessage>,

    next_stream_id: u64,
}

pub struct BoundConn {
    inner: Conn,
    _handle: std::thread::JoinHandle<()>,
}

impl Conn {
    pub fn new(addr: &str) -> anyhow::Result<Self> {
        let (outgoing_tx, outgoing_rx) = crossbeam::unbounded();
        let (incoming_tx, incoming_rx) = crossbeam::unbounded();

        let mut inner = InnerConn::new(addr, incoming_tx, outgoing_rx)?;
        let waker = inner.waker();

        // Start the connection thread.
        let thread_handle = std::thread::Builder::new()
            .name("quic conn".to_string())
            .spawn(move || inner.run())?;

        Ok(Self {
            thread_handle,
            waker,
            incoming: incoming_rx,
            outgoing: outgoing_tx,
            next_stream_id: 0,
        })
    }

    pub fn send(
        &mut self,
        msg: impl Into<protocol::MessageType>,
        sid: Option<u64>,
        fin: bool,
    ) -> anyhow::Result<u64> {
        let sid = match sid {
            Some(v) => v,
            None => {
                let sid = self.next_stream_id;
                self.next_stream_id += 4;
                sid
            }
        };

        match self.outgoing.send(OutgoingMessage {
            sid,
            msg: msg.into(),
            fin,
        }) {
            Ok(()) => {}
            Err(_) => {
                bail!("connection closed");
            }
        }

        self.waker.wake()?;
        Ok(sid)
    }

    /// Sends a message on a new stream, then waits for a reply on the same
    /// stream with a timeout. This is not suitable for concurrent use - it
    /// expects to only receive the reply, and no other messages.
    #[instrument(skip(self), level = "trace")]
    pub fn blocking_request(
        &mut self,
        msg: impl Into<protocol::MessageType> + std::fmt::Debug,
        timeout: time::Duration,
    ) -> anyhow::Result<protocol::MessageType> {
        let new_sid = self.send(msg.into(), None, true)?;

        loop {
            match self.incoming.recv_timeout(timeout) {
                Ok(ConnEvent::StreamMessage(sid, msg)) if sid == new_sid => return Ok(msg),
                Ok(ConnEvent::StreamMessage(_, m)) => {
                    bail!("received unexpected {}", m);
                }
                Ok(ConnEvent::StreamClosed(sid)) if sid == new_sid => {
                    bail!("stream closed by peer");
                }
                Err(e) => {
                    if e.is_timeout() {
                        bail!("timed out waiting for response");
                    } else {
                        return Err(e).context("connection closed");
                    }
                }
                _ => continue,
            }
        }
    }

    /// Binds to a winit event loop, proxying incoming messages to the given
    /// EventLoopProxy.
    pub fn bind_event_loop<T>(self, proxy: winit::event_loop::EventLoopProxy<T>) -> BoundConn
    where
        T: From<ConnEvent> + Send,
    {
        let incoming = self.incoming.clone();
        let handle = std::thread::spawn(move || {
            while let Ok(msg) = incoming.recv() {
                match proxy.send_event(msg.into()) {
                    Ok(()) => {}
                    Err(_) => {
                        break;
                    }
                }
            }

            proxy.send_event(ConnEvent::ConnectionClosed.into()).ok();
        });

        BoundConn {
            inner: self,
            _handle: handle,
        }
    }

    pub fn close(self) -> anyhow::Result<()> {
        drop(self.outgoing);
        self.waker.wake().ok();

        match self.thread_handle.join() {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(anyhow!("connection thread panicked")),
        }
    }
}

impl BoundConn {
    pub fn send(
        &mut self,
        msg: impl Into<protocol::MessageType>,
        sid: Option<u64>,
        fin: bool,
    ) -> anyhow::Result<u64> {
        self.inner.send(msg, sid, fin)
    }

    pub fn close(self) -> anyhow::Result<()> {
        self.inner.close()
    }
}

struct InnerConn {
    scratch: bytes::BytesMut,
    socket: mio::net::UdpSocket,
    local_addr: SocketAddr,
    poll: mio::Poll,
    waker: Arc<mio::Waker>,
    conn: quiche::Connection,
    partial_reads: HashMap<u64, bytes::BytesMut>,
    open_streams: HashSet<u64>,
    shutting_down: bool,
    stats_timer: time::Instant,

    incoming: crossbeam::Sender<ConnEvent>,
    outgoing: crossbeam::Receiver<OutgoingMessage>,
}

const MAX_QUIC_PACKET_SIZE: usize = 1350;

const SOCKET: mio::Token = mio::Token(0);
const WAKER: mio::Token = mio::Token(1);

impl InnerConn {
    pub fn new(
        addr: &str,
        incoming: crossbeam::Sender<ConnEvent>,
        outgoing: crossbeam::Receiver<OutgoingMessage>,
    ) -> anyhow::Result<Self> {
        let (hostname, server_addr) = resolve_server(addr)?;
        let bind_addr = match server_addr {
            std::net::SocketAddr::V4(_) => "0.0.0.0:0",
            std::net::SocketAddr::V6(_) => "[::]:0",
        };

        let mut socket = mio::net::UdpSocket::bind(bind_addr.parse()?)?;

        let mut config = quiche::Config::new(quiche::PROTOCOL_VERSION)?;

        if !ip_rfc::global(&server_addr.ip()) {
            warn!("skipping TLS verification for private server address");
            config.verify_peer(false);
        }

        config.set_application_protos(&[b"mm00"])?;

        config.set_max_idle_timeout(30_000);
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
            shutting_down: false,
            stats_timer: time::Instant::now(),

            incoming,
            outgoing,
        })
    }

    pub fn waker(&self) -> Arc<mio::Waker> {
        self.waker.clone()
    }

    pub fn run(&mut self) -> anyhow::Result<()> {
        let mut events = mio::Events::with_capacity(1024);

        loop {
            self.poll.poll(&mut events, self.conn.timeout())?;

            let now = time::Instant::now();
            if let Some(timeout_instant) = self.conn.timeout_instant() {
                if now >= timeout_instant {
                    self.conn.on_timeout();
                }
            }

            if self.conn.is_closed() || self.conn.is_draining() {
                if self.conn.is_timed_out() {
                    bail!("connection timed out");
                } else if self.conn.is_dgram_recv_queue_full() {
                    bail!("dgram recv queue full");
                } else if let Some(err) = self.conn.peer_error() {
                    bail!("closed by peer: {:?}", err);
                } else if !self.shutting_down {
                    bail!("closing unexpectedly")
                } else {
                    return Ok(());
                }
            }

            if (now - self.stats_timer) > time::Duration::from_millis(200) {
                self.stats_timer = now;
                let stats = self.conn.path_stats().next().unwrap();
                STATS.set_rtt(stats.rtt);
            }

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
                            let (msg, msg_len) = protocol::decode_message(&self.scratch)
                                .context("error reading datagram from server")?;
                            debug_assert_eq!(msg_len, len);

                            trace!(%msg, len, "received datagram");

                            match self.incoming.send(ConnEvent::Datagram(msg)) {
                                Ok(()) => {}
                                Err(_) => {
                                    self.conn.close(true, 0x00, b"")?;
                                    self.shutting_down = true;
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
                            self.open_streams.insert(sid);
                            self.send_message(sid, msg, fin)?;
                        }
                        Err(crossbeam::TryRecvError::Empty) => break,
                        Err(crossbeam::TryRecvError::Disconnected) => {
                            self.conn.close(true, 0x00, b"")?;
                            self.shutting_down = true;
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
                            self.conn.close(true, 0x00, b"")?;
                            self.shutting_down = true;
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

    fn pump_stream(&mut self, sid: u64) -> anyhow::Result<bool> {
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
                    self.conn.close(true, 0x00, b"")?;
                    self.shutting_down = true;
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
    ) -> anyhow::Result<()> {
        self.scratch.resize(protocol::MAX_MESSAGE_SIZE, 0);
        let len = protocol::encode_message(&msg, &mut self.scratch)?;

        trace!(sid, %msg, "sending message");
        self.conn
            .stream_send(sid, &self.scratch[..len], fin)
            .context("quiche stream_send error")?;

        Ok(())
    }
}

fn gen_scid() -> quiche::ConnectionId<'static> {
    use ring::rand::SecureRandom;

    let mut scid = vec![0; quiche::MAX_CONN_ID_LEN];

    ring::rand::SystemRandom::new().fill(&mut scid[..]).unwrap();
    quiche::ConnectionId::from_vec(scid)
}

fn resolve_server(hostport: &str) -> anyhow::Result<(String, SocketAddr)> {
    use std::net::ToSocketAddrs;

    let parts = hostport.splitn(2, ':').collect::<Vec<_>>();
    let (host, port) = match parts[..] {
        [host] => {
            debug!("assuming default port {}", DEFAULT_PORT);
            (host, DEFAULT_PORT)
        }
        [host, port] => {
            let port: u16 = port.parse().context("invalid destination address")?;
            (host, port)
        }
        _ => {
            bail!("invalid destination address");
        }
    };

    let addr = (host, port)
        .to_socket_addrs()
        .context("invalid destination address")?
        .next()
        .unwrap();

    Ok((host.to_string(), addr))
}
