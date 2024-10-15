// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

mod handlers;
mod sendmmsg;

use std::collections::{BTreeMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::anyhow;
use anyhow::bail;
use anyhow::Context;
use bytes::{Buf, Bytes, BytesMut};
use crossbeam_channel::{Receiver, Sender, TryRecvError};
use hashbrown::HashMap;
use mm_protocol as protocol;
use protocol::error::ErrorCode;
use ring::rand::{self, SecureRandom};
use tracing::debug_span;
use tracing::trace;
use tracing::trace_span;
use tracing::warn;
use tracing::{debug, error};

use crate::state::SharedState;
use crate::waking_sender::WakingOneshot;
use crate::waking_sender::WakingSender;

const MAX_QUIC_PACKET_SIZE: usize = 1350;

const SOCKET: mio::Token = mio::Token(0);
const WAKER: mio::Token = mio::Token(1);

pub struct Server {
    server_config: crate::config::ServerConfig,
    quiche_config: quiche::Config,
    addr: SocketAddr,
    socket: mio::net::UdpSocket,
    scratch: BytesMut,
    outgoing_packets: VecDeque<Outgoing>,

    poll: mio::Poll,
    waker: Arc<mio::Waker>,
    next_timer_token: usize,
    thread_pool: threadpool::ThreadPool,

    clients: HashMap<quiche::ConnectionId<'static>, ClientConnection>,
    state: SharedState,
    close_recv: Receiver<()>,
    close_send: WakingSender<()>,
    shutting_down: bool,
}

struct Outgoing {
    buf: Bytes,
    to: SocketAddr,
}

pub struct StreamWorker {
    incoming_messages: Option<Sender<protocol::MessageType>>,
    outgoing_messages: Receiver<protocol::MessageType>,
    done: oneshot::Receiver<()>,
}

pub struct ClientConnection {
    remote_addr: SocketAddr,
    conn_id: quiche::ConnectionId<'static>,
    conn: quiche::Connection,
    timer: mio_timerfd::TimerFd,
    timeout_token: mio::Token,
    partial_reads: BTreeMap<u64, BytesMut>,
    partial_writes: BTreeMap<u64, Bytes>,
    in_flight: BTreeMap<u64, StreamWorker>,
    dgram_recv: Receiver<protocol::MessageType>,
    dgram_send: WakingSender<protocol::MessageType>,
}

impl Server {
    pub fn new(
        socket: std::net::UdpSocket,
        server_config: crate::config::ServerConfig,
        state: SharedState,
    ) -> anyhow::Result<Self> {
        let poll = mio::Poll::new().unwrap();
        let waker = Arc::new(mio::Waker::new(poll.registry(), WAKER)?);

        let clients = HashMap::new();

        let mut config = match (&server_config.tls_cert, &server_config.tls_key) {
            (Some(cert), Some(key)) => {
                let mut config = quiche::Config::new(quiche::PROTOCOL_VERSION)?;

                config
                    .load_cert_chain_from_pem_file(cert.to_str().unwrap())
                    .context("loading certificate file")?;
                config
                    .load_priv_key_from_pem_file(key.to_str().unwrap())
                    .context("loading private key file")?;
                config
            }
            _ => {
                let tls_ctx = self_signed_tls_ctx(socket.local_addr()?)?;
                quiche::Config::with_boring_ssl_ctx_builder(quiche::PROTOCOL_VERSION, tls_ctx)?
            }
        };

        config.set_application_protos(&[protocol::ALPN_PROTOCOL_VERSION])?;
        config.set_max_idle_timeout(10_000);
        config.set_initial_max_data(65536);
        config.set_initial_max_stream_data_bidi_remote(65536);
        config.set_initial_max_stream_data_bidi_local(65536);
        config.set_initial_max_stream_data_uni(65536);
        config.set_initial_max_streams_bidi(64);
        config.set_initial_max_streams_uni(64);
        config.enable_dgram(true, 0, 1024 * 1024);
        config.set_cc_algorithm_name("bbr")?;
        config.enable_early_data();

        // Storage for packets that would have blocked on sending.
        let outgoing_packets = VecDeque::new();

        socket.set_nonblocking(true)?;
        sendmmsg::set_so_txtime(&socket)?;
        let mut socket = mio::net::UdpSocket::from_std(socket);
        poll.registry()
            .register(&mut socket, SOCKET, mio::Interest::READABLE)?;

        let (close_send, close_recv) = crossbeam_channel::bounded(1);
        let close_send = WakingSender::new(waker.clone(), close_send);

        let thread_pool = threadpool::ThreadPool::new(server_config.worker_threads.get() as usize);

        Ok(Self {
            server_config,
            quiche_config: config,
            addr: socket.local_addr()?,
            socket,
            scratch: BytesMut::with_capacity(65536),
            outgoing_packets,

            poll,
            waker,
            next_timer_token: 1024,
            thread_pool,

            clients,
            state,
            close_send,
            close_recv,
            shutting_down: false,
        })
    }

    pub fn local_addr(&self) -> anyhow::Result<SocketAddr> {
        Ok(self.socket.local_addr()?)
    }

    pub fn closer(&self) -> WakingSender<()> {
        self.close_send.clone()
    }

    /// Starts the server loop, returning only on error.
    pub fn run(&mut self) -> anyhow::Result<()> {
        let mut events = mio::Events::with_capacity(1024);

        'poll: loop {
            // TODO: It might be worthwhile to switch to a busy loop if
            // there are any active sessions. That would mean handling quiche
            // timeouts in userspace.
            match self.poll.poll(&mut events, None) {
                Ok(_) => (),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }

            // Check if we're supposed to shut down.
            if let Ok(()) = self.close_recv.try_recv() {
                debug!("shutting down server");
                self.shutting_down = true;
                for client in self.clients.values_mut() {
                    match client.conn.close(true, 0, &[]) {
                        Ok(_) | Err(quiche::Error::Done) => (),
                        Err(e) => {
                            bail!("failed to close connection: {:?}", e);
                        }
                    }
                }
            }

            for event in events.iter() {
                // Check if the token is a timeout token.
                let client = self
                    .clients
                    .values_mut()
                    .find(|c| c.timeout_token == event.token());
                if let Some(client) = client {
                    client.timer.read()?;
                    client.conn.on_timeout();
                    client.update_timeout()?;
                }
            }

            // Garbage-collect dead sessions.
            self.state.lock().tick()?;

            // Garbage-collect closed clients.
            self.clients.retain(|_, c| {
                if c.conn.is_closed() {
                    debug!(conn_id = ?c.conn_id, remote_addr = ?c.remote_addr, "client disconnected");
                    false
                } else if c.conn.is_draining() {
                    // Drop the workers, which drops the send/recv channels,
                    // signaling that the workers can exit already.
                    c.in_flight.clear();
                    true
                } else {
                    true
                }
            });

            if self.shutting_down && self.clients.is_empty() {
                return Ok(());
            } else if self.shutting_down {
                debug!("waiting for {} clients to disconnect", self.clients.len());
            }

            // Read incoming UDP packets and handle them.
            'read: loop {
                self.scratch.resize(MAX_QUIC_PACKET_SIZE, 0);
                let (len, from) = match self.socket.recv_from(&mut self.scratch) {
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        break 'read;
                    }
                    v => v.context("recv_from error")?,
                };

                let pkt = self.scratch.split_to(len);
                match self.recv(pkt, from) {
                    Ok(_) => {}
                    Err(e) => {
                        error!("recv failed: {:?}", e);
                    }
                }
            }

            // Write out any queued packets.
            while !self.outgoing_packets.is_empty() {
                let pkt = self.outgoing_packets.pop_front().unwrap();
                match self.socket.send_to(&pkt.buf, pkt.to) {
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        self.outgoing_packets.push_front(pkt);
                        continue 'poll;
                    }
                    v => v?,
                };
            }

            // Let workers know if any peers hung up, and let peers know if any
            // workers finished.
            for client in self.clients.values_mut() {
                let mut to_close = Vec::new();
                for (sid, worker) in client.in_flight.iter_mut() {
                    if client.conn.stream_finished(*sid) {
                        trace!("peer hung up on stream {:?}:{}", client.conn_id, sid);
                        worker.incoming_messages.take();
                    }

                    if matches!(
                        worker.done.try_recv(),
                        Ok(()) | Err(oneshot::TryRecvError::Disconnected)
                    ) && worker.outgoing_messages.is_empty()
                    {
                        to_close.push(*sid);
                    }
                }

                for sid in to_close {
                    trace!(sid, "closing stream because worker finished");

                    let _ = client.conn.stream_send(sid, &[], true);
                    let _ = client.conn.stream_shutdown(sid, quiche::Shutdown::Read, 0);
                    client.in_flight.remove(&sid);
                }
            }

            // Demux packets from in-flight requests and datagrams from attachments.
            for client in self.clients.values_mut() {
                let conn_span = trace_span!("conn_write", conn_id = ?client.conn_id);
                let _guard = conn_span.enter();

                if client.conn.is_draining() {
                    continue;
                }

                loop {
                    if client.conn.is_dgram_send_queue_full() {
                        warn!("datagram send queue full!");
                        break;
                    }

                    let msg = match client.dgram_recv.try_recv() {
                        Ok(msg) => msg,
                        Err(TryRecvError::Disconnected) => unreachable!(),
                        Err(TryRecvError::Empty) => break,
                    };

                    match client.send_dgram(msg, &mut self.scratch) {
                        Ok(_) => {}
                        Err(e) => {
                            match e.downcast_ref::<quiche::Error>() {
                                Some(quiche::Error::Done) => (),
                                _ => error!("failed to send datagram: {}", e),
                            }

                            client
                                .conn
                                .close(true, ErrorCode::ErrorProtocol as u64, &[])
                                .ok();
                            break;
                        }
                    }
                }

                for sid in client.conn.writable() {
                    if !client.in_flight.contains_key(&sid) {
                        continue;
                    }

                    if !client.flush_partial_write(sid)? {
                        continue;
                    }

                    loop {
                        let span = trace_span!("stream_write", sid);
                        let _guard = span.enter();

                        match client
                            .in_flight
                            .get(&sid)
                            .unwrap()
                            .outgoing_messages
                            .try_recv()
                        {
                            Ok(msg) => {
                                if !client.write_message(sid, msg, false, &mut self.scratch)? {
                                    // No more write capacity at the moment.
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                }
            }

            // Generate outgoing QUIC packets.
            let mut packets = Vec::new();

            let mut off = 0;
            for client in self.clients.values_mut() {
                loop {
                    let start = off;
                    self.scratch.resize(off + MAX_QUIC_PACKET_SIZE, 0);
                    let (len, send_info) = match client.conn.send(&mut self.scratch[off..]) {
                        Ok(v) => v,
                        Err(quiche::Error::Done) => break,
                        Err(e) => {
                            error!("QUIC error: {:?}", e);
                            continue;
                        }
                    };

                    off += len;
                    packets.push((start..(start + len), send_info.to, send_info.at));
                }

                // Update the timeout.
                client.update_timeout()?;
            }

            // Send out the packets.
            if !packets.is_empty() {
                let mut sendmmsg = sendmmsg::new();
                for (range, to, txtime) in packets {
                    sendmmsg = sendmmsg.sendmsg(&self.scratch[range], to, txtime);
                }

                sendmmsg.finish(&self.socket)?;
            }
        }
    }

    /// Handles an incoming datagram.
    fn recv(&mut self, mut pkt: BytesMut, from: SocketAddr) -> anyhow::Result<()> {
        let hdr = match quiche::Header::from_slice(&mut pkt, quiche::MAX_CONN_ID_LEN) {
            Ok(v) => v,
            Err(e) => {
                bail!("invalid packet: {:?}", e);
            }
        };

        let num_clients = self.clients.len();
        let client = match self.clients.get_mut(&hdr.dcid) {
            Some(c) => c,
            None if self.shutting_down => return Ok(()),
            None => {
                if hdr.ty != quiche::Type::Initial {
                    debug!("invalid packet: dcid not found and not Initial");
                    return Ok(());
                }

                if let Some(max) = self.server_config.max_connections {
                    if num_clients as u32 >= max.get() {
                        warn!("rejecting connection: max_connections ({}) reached", max);
                        return Ok(());
                    }
                }

                if !quiche::version_is_supported(hdr.version) {
                    debug!(
                        "version {:x} is not supported; doing version negotiation",
                        hdr.version
                    );

                    let out = {
                        self.scratch.resize(MAX_QUIC_PACKET_SIZE, 0);
                        let len =
                            quiche::negotiate_version(&hdr.scid, &hdr.dcid, &mut self.scratch)?;
                        self.scratch.split_to(len).freeze()
                    };

                    self.outgoing_packets
                        .push_back(Outgoing { buf: out, to: from });
                    return Ok(());
                }

                let conn_id = gen_random_cid();
                let conn =
                    quiche::accept(&conn_id, None, self.addr, from, &mut self.quiche_config)?;

                let mut timer = mio_timerfd::TimerFd::new(mio_timerfd::ClockId::Monotonic)?;
                let timeout_token = mio::Token(self.next_timer_token);
                self.next_timer_token += 1;
                self.poll.registry().register(
                    &mut timer,
                    timeout_token,
                    mio::Interest::READABLE,
                )?;

                let streams = BTreeMap::new();

                let (dgram_send, dgram_recv) = crossbeam_channel::unbounded();
                let dgram_send = WakingSender::new(self.waker.clone(), dgram_send);

                let c = ClientConnection {
                    remote_addr: from,
                    conn_id: conn_id.clone(),
                    conn,
                    timer,
                    timeout_token,
                    in_flight: streams,
                    partial_reads: BTreeMap::new(),
                    partial_writes: BTreeMap::new(),
                    dgram_recv,
                    dgram_send,
                };

                debug!("new client connection: {}", from);
                self.clients.entry(conn_id).or_insert(c)
            }
        };

        // Run QUIC machinery.
        client.conn.recv(
            &mut pkt,
            quiche::RecvInfo {
                from,
                to: self.addr,
            },
        )?;

        for sid in client.conn.readable() {
            let (messages, fin) = match client.read_messages(sid, &mut self.scratch) {
                Ok(v) => v,
                Err(e) => {
                    if e.downcast_ref::<protocol::ProtocolError>().is_some() {
                        client.err_stream(
                            sid,
                            ErrorCode::ErrorProtocol,
                            Some(e.to_string()),
                            &mut self.scratch,
                        );
                    } else {
                        error!("unexpected error: {}", e);
                        client.err_stream(
                            sid,
                            ErrorCode::ErrorServer,
                            Some("Internal server error".to_string()),
                            &mut self.scratch,
                        );
                    }

                    continue;
                }
            };

            let worker = match client.in_flight.get_mut(&sid) {
                Some(w) => w,
                None if messages.is_empty() => continue,
                None => {
                    let (incoming_send, incoming_recv) = crossbeam_channel::unbounded();
                    let (outgoing_send, outgoing_recv) = crossbeam_channel::unbounded();
                    let outgoing_send = WakingSender::new(self.waker.clone(), outgoing_send);
                    let outgoing_dgrams = client.dgram_send.clone();

                    let (done_send, done_recv) = oneshot::channel();
                    let done_send = WakingOneshot::new(self.waker.clone(), done_send);

                    let state_clone = self.state.clone();
                    let max_dgram_len = match client.conn.dgram_max_writable_len() {
                        Some(v) => v,
                        None => bail!("client doesn't support datagrams"),
                    };

                    let client_addr = client.remote_addr;
                    self.thread_pool.execute(move || {
                        let span = debug_span!("stream", sid, remote_addr = ?client_addr);
                        let _guard = span.enter();

                        handlers::dispatch(
                            state_clone,
                            incoming_recv,
                            outgoing_send,
                            outgoing_dgrams,
                            max_dgram_len,
                            done_send,
                        );
                    });

                    let worker = StreamWorker {
                        incoming_messages: Some(incoming_send),
                        outgoing_messages: outgoing_recv,
                        done: done_recv,
                    };

                    client.in_flight.entry(sid).or_insert(worker)
                }
            };

            let incoming = worker.incoming_messages.as_ref().unwrap();
            for msg in messages {
                if incoming.send(msg).is_err() {
                    // The worker finished execution, so ignore any further
                    // messages.
                    break;
                }
            }

            if fin {
                // Signal to the worker that the peer has stopped sending
                // messages.
                worker.incoming_messages.take();
            }
        }

        // Update the timeout timer.
        client.update_timeout()?;

        // Clean up partial data for closed streams.
        client
            .partial_reads
            .retain(|sid, _| !client.conn.stream_finished(*sid));
        client
            .partial_writes
            .retain(|sid, _| !client.conn.stream_finished(*sid));

        Ok(())
    }
}

fn self_signed_tls_ctx(addr: SocketAddr) -> anyhow::Result<boring::ssl::SslContextBuilder> {
    use boring::pkey::PKey;
    use boring::x509::X509;

    let ip = addr.ip();
    assert!(!ip_rfc::global(&ip) && !ip.is_unspecified());

    let certs = rcgen::generate_simple_self_signed(vec![ip.to_string()])
        .context("generating self-signed certificates")?;

    let cert = X509::from_pem(certs.serialize_pem()?.as_bytes())?;
    let key = PKey::private_key_from_pem(certs.serialize_private_key_pem().as_bytes())?;

    let mut tls_ctx = boring::ssl::SslContextBuilder::new(boring::ssl::SslMethod::tls())?;
    tls_ctx.set_private_key(&key)?;
    tls_ctx.set_certificate(&cert)?;

    Ok(tls_ctx)
}

impl ClientConnection {
    fn update_timeout(&mut self) -> anyhow::Result<()> {
        if let Some(new_timeout) = self.conn.timeout() {
            self.timer.set_timeout(&new_timeout)?;
        } else {
            self.timer.disarm()?;
        }

        Ok(())
    }

    fn read_messages(
        &mut self,
        sid: u64,
        scratch: &mut BytesMut,
    ) -> anyhow::Result<(Vec<protocol::MessageType>, bool)> {
        // Start with partial data from the previous call to read_messages.
        scratch.truncate(0);
        if let Some(partial) = self.partial_reads.remove(&sid) {
            scratch.unsplit(partial);
        }

        let mut off = scratch.len();
        let mut stream_fin = false;
        loop {
            scratch.resize(off + protocol::MAX_MESSAGE_SIZE, 0);
            match self.conn.stream_recv(sid, &mut scratch[off..]) {
                Ok((len, fin)) => {
                    off += len;

                    if fin {
                        stream_fin = true;
                        break;
                    }
                }
                Err(quiche::Error::Done) => {
                    break;
                }
                Err(e) => return Err(e.into()),
            }
        }

        // Read messages (there may be multiple).
        scratch.truncate(off);
        let mut buf = scratch.split();
        let mut messages = Vec::new();
        while !buf.is_empty() {
            match protocol::decode_message(&buf) {
                Ok((msg, len)) => {
                    trace!(
                        conn_id = ?self.conn_id,
                        stream_id = sid,
                        len,
                        "received {}", msg
                    );

                    messages.push(msg);
                    buf.advance(len);
                }
                Err(protocol::ProtocolError::InvalidMessageType(t, len)) => {
                    warn!(msgtype = t, len, "ignoring unknown message type");
                    buf.advance(len);
                }
                Err(protocol::ProtocolError::ShortBuffer(n)) => {
                    trace!(
                        "partial message on stream {:?}:{}, need {} bytes",
                        self.conn_id,
                        sid,
                        n
                    );

                    self.partial_reads.insert(sid, buf);
                    break;
                }
                Err(e) => return Err(e.into()),
            };
        }

        Ok((messages, stream_fin))
    }

    /// Send a message on a stream. Returns Ok(false) if the stream is full.
    fn write_message(
        &mut self,
        sid: u64,
        msg: protocol::MessageType,
        fin: bool,
        scratch: &mut BytesMut,
    ) -> anyhow::Result<bool> {
        scratch.resize(protocol::MAX_MESSAGE_SIZE, 0);
        let len = protocol::encode_message(&msg, scratch)?;

        trace!(len, "sending {}", msg);

        match self.conn.stream_send(sid, &scratch[..len], fin) {
            Ok(n) if n != len => {
                // Partial write.
                assert!(n < len);
                trace!(n, "partial write");

                let partial = scratch.split_to(len).split_off(n).freeze();
                let old = self.partial_writes.insert(sid, partial);
                assert_eq!(None, old);

                Ok(false)
            }
            Err(quiche::Error::Done) => {
                trace!("stream blocked");

                let data = scratch.split_to(len).freeze();
                let old = self.partial_writes.insert(sid, data);
                assert_eq!(None, old);

                Ok(false)
            }
            v => {
                assert_eq!(len, v?);
                Ok(true)
            }
        }
    }

    /// Flushes previous partial writes.
    fn flush_partial_write(&mut self, sid: u64) -> anyhow::Result<bool> {
        use std::collections::btree_map::Entry;

        match self.partial_writes.entry(sid) {
            Entry::Vacant(_) => Ok(true),
            Entry::Occupied(mut entry) => {
                let partial = entry.get().clone();
                trace!(len = partial.len(), "flushing previous partial");

                match self.conn.stream_send(sid, &partial, false) {
                    Ok(n) if n != entry.get().len() => {
                        // Partial write.
                        entry.get_mut().advance(n);
                        trace!(len = entry.get().len(), "remaining partial");
                        Ok(false)
                    }
                    Ok(_) => {
                        entry.remove();
                        Ok(true)
                    }
                    Err(quiche::Error::Done) => Ok(false),
                    Err(e) => Err(anyhow!(e)),
                }
            }
        }
    }

    /// Send a message as a datagram.
    fn send_dgram(
        &mut self,
        msg: protocol::MessageType,
        scratch: &mut BytesMut,
    ) -> anyhow::Result<()> {
        #[cfg(debug_assertions)]
        match msg {
            protocol::MessageType::VideoChunk(_) | protocol::MessageType::AudioChunk(_) => {}
            _ => panic!("received non-dgram message on dgram channel"),
        }

        scratch.resize(protocol::MAX_MESSAGE_SIZE, 0);
        let len = protocol::encode_message(&msg, scratch).unwrap();

        trace!(
            conn_id = ?self.conn_id,
            len,
            "sending datagram {}", msg
        );

        match self.conn.dgram_send_vec(scratch[..len].to_vec()) {
            Ok(_) => Ok(()),
            Err(quiche::Error::InvalidState) => Err(anyhow!("client doesn't support datagrams")),
            Err(e) => Err(e.into()),
        }
    }

    /// Send an Error message on a stream, then shut it down.
    fn err_stream(
        &mut self,
        sid: u64,
        code: ErrorCode,
        error: Option<String>,
        scratch: &mut BytesMut,
    ) {
        // TODO actually send an error message
        let msg = protocol::Error {
            error_text: error.unwrap_or_default(),
            err_code: code.into(),
        };

        let _ = self.write_message(sid, msg.into(), true, scratch);
        let _ = self
            .conn
            .stream_shutdown(sid, quiche::Shutdown::Read, code as u64);

        self.in_flight.remove(&sid);
    }
}

fn gen_random_cid() -> quiche::ConnectionId<'static> {
    let mut cid = vec![0; quiche::MAX_CONN_ID_LEN];
    let rng = rand::SystemRandom::new();
    rng.fill(&mut cid).unwrap();
    quiche::ConnectionId::from_vec(cid)
}
