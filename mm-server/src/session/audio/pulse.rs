// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{
    collections::BTreeMap,
    ffi::{CStr, CString},
    io::{prelude::*, Cursor},
    path::Path,
    sync::Arc,
    time,
};

use anyhow::{bail, Context};
use bytes::BytesMut;
use crossbeam_channel as crossbeam;
use cstr::cstr;
use mio::net::UnixListener;
use pulseaudio::protocol::{self as pulse, ClientInfoList};
use tracing::{debug, error, trace, warn};

use super::buffer::PlaybackBuffer;
use super::EncodeFrame;
use crate::{session::EPOCH, waking_sender::WakingSender};

const WAKER: mio::Token = mio::Token(0);
const LISTENER: mio::Token = mio::Token(1);
const CLOCK: mio::Token = mio::Token(2);

// The server emits samples at this rate to the encoder.
pub const CAPTURE_SAMPLE_RATE: u32 = 48000;
pub const CAPTURE_CHANNEL_COUNT: u32 = 2;
pub const CAPTURE_SPEC: pulse::SampleSpec = pulse::SampleSpec {
    format: pulse::SampleFormat::Float32Le,
    channels: CAPTURE_CHANNEL_COUNT as u8,
    sample_rate: CAPTURE_SAMPLE_RATE,
};

// Run the clock every 10ms, which is the smallest Opus frame size.
const CLOCK_RATE_HZ: u32 = 100;

const SINK_NAME: &CStr = cstr!("magic_mirror");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamState {
    Prebuffering(u64), // The number of bytes remaining before we can start 'playback'.
    Corked,
    Playing,
    Draining(u32), // The seq of the drain request, so we can ack it.
}

struct PlaybackStream {
    state: StreamState,
    buffer_attr: pulse::stream::BufferAttr,
    buffer: PlaybackBuffer<[f32; 2]>,
    requested_bytes: usize,
    played_bytes: u64,
    write_offset: u64,
    read_offset: u64,
}

struct Client {
    id: u32,
    socket: mio::net::UnixStream,
    protocol_version: u16,
    props: Option<pulse::Props>,
    incoming: BytesMut,
    playback_streams: BTreeMap<u32, PlaybackStream>,
}

struct ServerState {
    server_info: pulse::ServerInfo,
    cards: Vec<pulse::CardInfo>,
    sinks: Vec<pulse::SinkInfo>,
    default_format_info: pulse::FormatInfo,
    next_playback_channel_index: u32,
}

pub struct PulseServer {
    listener: UnixListener,
    poll: mio::Poll,
    clock: mio_timerfd::TimerFd,

    close_rx: crossbeam::Receiver<()>,
    unencoded_tx: crossbeam::Sender<EncodeFrame>,
    done_rx: crossbeam::Receiver<EncodeFrame>,

    clients: BTreeMap<mio::Token, Client>,
    server_state: ServerState,
}

impl PulseServer {
    pub fn new(
        socket_name: impl AsRef<Path>,
        unencoded_tx: crossbeam::Sender<super::EncodeFrame>,
        done_rx: crossbeam::Receiver<super::EncodeFrame>,
    ) -> anyhow::Result<(Self, WakingSender<()>)> {
        let listener = UnixListener::bind(socket_name)?;
        let poll = mio::Poll::new()?;
        let waker = Arc::new(mio::Waker::new(poll.registry(), WAKER)?);

        let mut clock = mio_timerfd::TimerFd::new(mio_timerfd::ClockId::Monotonic)?;
        clock.set_timeout_interval(&time::Duration::from_nanos(
            1_000_000_000 / CLOCK_RATE_HZ as u64,
        ))?;

        let mut server_info = pulse::ServerInfo {
            server_name: Some(cstr!("Magic Mirror").into()),
            server_version: Some(cstr!("0.0.1").into()),
            host_name: Some(CString::new("mmserver")?),
            default_sink_name: Some(SINK_NAME.into()),
            default_source_name: Some(SINK_NAME.into()),
            ..Default::default()
        };

        // let dummy_card_index = 99;
        // let mut dummy_card = pulse::CardInfo {
        //     index: dummy_card_index,
        //     name: cstr!("Magic Mirror virtual output").into(),
        //     props: pulse::Props::new(),
        //     owner_module_index: None,
        //     driver: Some(cstr!("magic-mirror").into()),
        //     ports: vec![pulse::CardPortInfo {
        //         name: cstr!("virtual-output-0").into(),
        //         description: Some(cstr!("virtual output").into()),
        //         priority: 0,
        //         available: pulse::port_info::PortAvailable::Yes,
        //         dir: pulse::port_info::PortDirection::Input,
        //         props: pulse::Props::new(),
        //         port_type: pulse::port_info::PortType::Network,
        //         availability_group: None, //Some(cstr!("output").into()),
        //         profiles: vec![cstr!("output:stereo").into()],
        //         latency_offset: 0,
        //     }],
        //     profiles: vec![pulse::CardProfileInfo {
        //         name: cstr!("output:stereo").into(),
        //         description: Some(cstr!("Stereo").into()),
        //         priority: 1000,
        //         available: 1,
        //         num_sinks: 1,
        //         num_sources: 0,
        //     }],
        //     active_profile: Some(cstr!("output:stereo").into()),
        // };

        // dummy_card.props.set(
        //     pulse::Prop::DeviceDescription,
        //     cstr!("Magic Mirror virtual output"),
        // );

        let mut dummy_sink = pulse::SinkInfo::new_dummy(1);
        dummy_sink.name = SINK_NAME.into();
        dummy_sink.description = Some(cstr!("Magic Mirror virtual output").into());
        dummy_sink.sample_spec = pulse::SampleSpec {
            format: pulse::SampleFormat::Float32Le,
            channels: 2,
            sample_rate: CAPTURE_SAMPLE_RATE,
        };

        server_info.channel_map = dummy_sink.channel_map;
        server_info.sample_spec = dummy_sink.sample_spec;

        // dummy_sink.card_index = Some(dummy_card_index);
        dummy_sink.ports[0].port_type = pulse::port_info::PortType::Network;
        dummy_sink.ports[0].description = Some(cstr!("virtual output").into());

        let mut format_props = pulse::Props::new();
        format_props.set(pulse::Prop::FormatChannels, cstr!("2"));
        format_props.set(
            pulse::Prop::FormatChannelMap,
            cstr!("front-left,front-right"),
        );
        format_props.set(pulse::Prop::FormatSampleFormat, cstr!("float32le"));
        format_props.set(
            pulse::Prop::FormatRate,
            CString::new(CAPTURE_SAMPLE_RATE.to_string()).unwrap(),
        );

        let default_format_info = pulse::FormatInfo {
            encoding: pulse::FormatEncoding::Pcm,
            props: format_props,
        };

        dummy_sink.formats[0] = default_format_info.clone();

        let (close_tx, close_rx) = crossbeam::bounded(1);
        let close_tx = WakingSender::new(waker.clone(), close_tx);

        Ok((
            Self {
                listener,
                poll,
                clock,
                unencoded_tx,
                done_rx,
                close_rx,
                clients: BTreeMap::new(),
                server_state: ServerState {
                    server_info,
                    cards: vec![], // vec![dummy_card],
                    sinks: vec![dummy_sink],
                    default_format_info,
                    next_playback_channel_index: 0,
                },
            },
            close_tx,
        ))
    }

    pub fn run(&mut self) -> anyhow::Result<()> {
        // Client tokens start from 1024.
        let mut next_client_token = 1024;

        self.poll
            .registry()
            .register(&mut self.clock, CLOCK, mio::Interest::READABLE)?;

        self.poll
            .registry()
            .register(&mut self.listener, LISTENER, mio::Interest::READABLE)?;

        let mut events = mio::Events::with_capacity(1024);

        loop {
            match self
                .poll
                .poll(&mut events, Some(time::Duration::from_secs(1)))
            {
                Ok(_) => (),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }

            match self.close_rx.try_recv() {
                Ok(()) | Err(crossbeam::TryRecvError::Disconnected) => return Ok(()),
                _ => (),
            }

            for event in events.iter() {
                match event.token() {
                    CLOCK => {
                        self.clock.read()?;
                        self.clock_tick()?;
                    }
                    LISTENER => {
                        let (mut socket, _) = self.listener.accept()?;
                        let id = next_client_token as u32;
                        let token = mio::Token(next_client_token);
                        next_client_token += 1;

                        debug!("pulseaudio client connected");

                        self.poll.registry().register(
                            &mut socket,
                            token,
                            mio::Interest::READABLE,
                        )?;

                        self.clients.insert(
                            token,
                            Client {
                                id,
                                socket,
                                protocol_version: pulse::MAX_VERSION,
                                props: None,
                                incoming: BytesMut::new(),
                                playback_streams: BTreeMap::new(),
                            },
                        );
                    }
                    client_token if event.is_read_closed() => {
                        if let Some(mut client) = self.clients.remove(&client_token) {
                            debug!("pulseaudio client disconnected");
                            self.poll.registry().deregister(&mut client.socket)?;
                        }
                    }
                    client_token
                        if event.is_readable() && self.clients.contains_key(&client_token) =>
                    {
                        if let Err(e) = self.recv(client_token) {
                            error!("pulseaudio client error: {}", e);
                            let mut client = self.clients.remove(&client_token).unwrap();
                            self.poll.registry().deregister(&mut client.socket)?;
                        }
                    }
                    _ => (),
                }
            }
        }
    }

    fn recv(&mut self, client_token: mio::Token) -> anyhow::Result<()> {
        let client = self.clients.get_mut(&client_token).unwrap();

        let mut read_size = 8192;

        'read: loop {
            let off = client.incoming.len();
            client.incoming.resize(off + read_size, 0);
            let n = match client.socket.read(&mut client.incoming[off..]) {
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    client.incoming.truncate(off);
                    return Ok(());
                }
                v => v.context("recv error")?,
            };

            client.incoming.truncate(off + n);

            loop {
                if client.incoming.len() < pulse::DESCRIPTOR_SIZE {
                    read_size = 8192;
                    continue 'read;
                }

                let desc = pulse::read_descriptor(&mut Cursor::new(
                    &client.incoming[..pulse::DESCRIPTOR_SIZE],
                ))?;
                if client.incoming.len() < (desc.length as usize + pulse::DESCRIPTOR_SIZE) {
                    read_size =
                        desc.length as usize + pulse::DESCRIPTOR_SIZE - client.incoming.len();
                    continue 'read;
                }

                let _desc_bytes = client.incoming.split_to(pulse::DESCRIPTOR_SIZE);
                let payload = client.incoming.split_to(desc.length as usize).freeze();

                if desc.channel == u32::MAX {
                    let (seq, cmd) = match pulse::Command::read_tag_prefixed(
                        &mut Cursor::new(payload),
                        client.protocol_version,
                    ) {
                        Err(pulse::ProtocolError::Unimplemented(seq, cmd)) => {
                            error!("received unimplemented command {:?}", cmd);

                            pulse::write_error(
                                &mut client.socket,
                                seq,
                                pulse::PulseError::NotImplemented,
                            )?;

                            continue;
                        }
                        v => v.context("decoding command")?,
                    };

                    match handle_command(client, &mut self.server_state, seq, cmd) {
                        Ok(()) => (),
                        Err(e) => {
                            let _ = pulse::write_error(
                                &mut client.socket,
                                seq,
                                pulse::PulseError::Internal,
                            );

                            return Err(e);
                        }
                    }
                } else {
                    handle_stream_write(client, desc, &payload)?;
                }
            }
        }
    }

    fn clock_tick(&mut self) -> anyhow::Result<()> {
        let mut done_draining = Vec::new();

        let capture_ts = EPOCH.elapsed().as_millis() as u64;
        let num_frames = CAPTURE_SAMPLE_RATE / CLOCK_RATE_HZ;
        let encode_len = num_frames * CAPTURE_CHANNEL_COUNT;

        let mut frame = match self.done_rx.try_recv() {
            Ok(mut frame) => {
                frame.buf.resize(encode_len as usize, 0.0);
                frame.buf.fill(0.0);
                Some(frame)
            }
            Err(crossbeam::TryRecvError::Empty) => {
                // No one's listening, but we still need to capture audio from
                // clients.
                None
            }
            Err(crossbeam::TryRecvError::Disconnected) => return Ok(()),
        };

        for client in self.clients.values_mut() {
            done_draining.clear();
            for (id, stream) in client.playback_streams.iter_mut() {
                if matches!(
                    stream.state,
                    StreamState::Playing | StreamState::Draining(_)
                ) {
                    // Track how much we read.
                    let buffer_len = stream.buffer.len_bytes();

                    // Check for underrun.
                    let Some(frames) = stream.buffer.drain(num_frames as usize) else {
                        error!(id, "buffer underrun for stream");
                        pulse::write_command_message(
                            &mut client.socket,
                            u32::MAX,
                            pulse::Command::Underflow(pulse::Underflow {
                                channel: *id,
                                offset: 0, // TODO
                            }),
                            client.protocol_version,
                        )?;

                        if stream.buffer_attr.pre_buffering > 0
                            && matches!(stream.state, StreamState::Playing)
                        {
                            stream.state =
                                StreamState::Prebuffering(stream.buffer_attr.pre_buffering as u64);
                            // TODO: request in this case?
                        }

                        continue;
                    };

                    if let Some(ref mut frame) = frame {
                        let mut resampled =
                            dasp::Signal::into_interleaved_samples(frames).into_iter();

                        for sample in &mut frame.buf {
                            *sample += resampled.next().unwrap_or_default();
                        }
                    } else {
                        // Discard data even if we're not encoding it.
                        drop(frames)
                    }

                    let read_len = buffer_len - stream.buffer.len_bytes();
                    trace!(
                        id,
                        read_len,
                        buffer_len,
                        new_len = buffer_len - read_len,
                        "stream read"
                    );

                    stream.read_offset += read_len as u64;
                    stream.played_bytes += read_len as u64;

                    // If we've drained the buffer, we can drop the stream.
                    if matches!(stream.state, StreamState::Draining(_)) && stream.buffer.is_empty()
                    {
                        debug!(id, "finished draining stream");
                        done_draining.push(*id)
                    }
                }

                // Request a write to fill the buffer.
                let bytes_needed = (stream.buffer_attr.target_length as usize)
                    .saturating_sub(stream.buffer.len_bytes() + stream.requested_bytes);
                if matches!(stream.state, StreamState::Playing | StreamState::Corked)
                    && bytes_needed >= stream.buffer_attr.minimum_request_length as usize
                {
                    trace!(id, bytes_needed, "requesting buffer write");

                    stream.requested_bytes += bytes_needed;
                    pulse::write_command_message(
                        &mut client.socket,
                        u32::MAX,
                        pulse::Command::Request(pulse::Request {
                            channel: *id,
                            length: bytes_needed as u32,
                        }),
                        client.protocol_version,
                    )?;
                }
            }

            for id in done_draining.iter() {
                let stream = client.playback_streams.remove(id).unwrap();
                if let StreamState::Draining(drain_seq) = stream.state {
                    pulse::write_ack_message(&mut client.socket, drain_seq)?;
                } else {
                    unreachable!()
                }
            }
        }

        // Encode the frame.
        if let Some(mut frame) = frame {
            frame.capture_ts = capture_ts;
            self.unencoded_tx.send(frame)?;
        }

        Ok(())
    }
}

fn handle_command(
    client: &mut Client,
    server: &mut ServerState,
    seq: u32,
    cmd: pulse::Command,
) -> anyhow::Result<()> {
    trace!("got command [{}]: {:#?}", seq, cmd);

    match cmd {
        pulse::Command::Auth(pulse::AuthParams { version, .. }) => {
            let version = std::cmp::min(version, pulse::MAX_VERSION);
            client.protocol_version = version;
            trace!("client protocol version: {}", version);

            write_reply(
                &mut client.socket,
                seq,
                &pulse::AuthReply {
                    version: pulse::MAX_VERSION,
                    ..Default::default()
                },
                client.protocol_version,
            )?;

            Ok(())
        }
        pulse::Command::SetClientName(props) => {
            client.props = Some(props);

            write_reply(
                &mut client.socket,
                seq,
                &pulse::SetClientNameReply {
                    client_id: client.id,
                },
                client.protocol_version,
            )?;

            Ok(())
        }
        // Introspection commands.
        pulse::Command::GetServerInfo => {
            write_reply(
                &mut client.socket,
                seq,
                &server.server_info,
                client.protocol_version,
            )?;
            Ok(())
        }
        pulse::Command::GetClientInfo(id) => {
            let reply = pulse::ClientInfo {
                index: id,
                ..Default::default()
            };

            write_reply(&mut client.socket, seq, &reply, client.protocol_version)?;
            Ok(())
        }
        pulse::Command::GetClientInfoList => {
            let reply: ClientInfoList = Vec::new(); // TODO
            write_reply(&mut client.socket, seq, &reply, client.protocol_version)?;
            Ok(())
        }
        pulse::Command::GetCardInfo(_) => {
            write_reply(
                &mut client.socket,
                seq,
                &server.cards[0],
                client.protocol_version,
            )?;

            Ok(())
        }
        pulse::Command::GetCardInfoList => {
            write_reply(
                &mut client.socket,
                seq,
                &server.cards,
                client.protocol_version,
            )?;

            Ok(())
        }
        pulse::Command::GetSinkInfo(_) => {
            write_reply(
                &mut client.socket,
                seq,
                &server.sinks[0],
                client.protocol_version,
            )?;

            Ok(())
        }
        pulse::Command::GetSinkInfoList => {
            write_reply(
                &mut client.socket,
                seq,
                &server.sinks,
                client.protocol_version,
            )?;

            Ok(())
        }
        pulse::Command::GetSinkInputInfoList => {
            let reply: pulse::SinkInputInfoList = Vec::new();
            write_reply(&mut client.socket, seq, &reply, client.protocol_version)?;
            Ok(())
        }
        pulse::Command::GetSourceInfo(_) => {
            pulse::write_error(&mut client.socket, seq, pulse::PulseError::NoEntity)?;
            Ok(())
        }
        pulse::Command::GetSourceOutputInfoList => {
            let reply: pulse::SourceOutputInfoList = Vec::new();
            write_reply(&mut client.socket, seq, &reply, client.protocol_version)?;
            Ok(())
        }
        pulse::Command::GetSourceInfoList => {
            let reply: pulse::SinkInfoList = Vec::new();
            write_reply(&mut client.socket, seq, &reply, client.protocol_version)?;
            Ok(())
        }
        pulse::Command::Subscribe(_) => {
            // We don't have any state changes that would warrant an event.
            pulse::write_ack_message(&mut client.socket, seq)?;
            Ok(())
        }
        // Playback streams.
        pulse::Command::CreatePlaybackStream(params) => {
            let mut sample_spec = params.sample_spec;
            if sample_spec.format == pulse::SampleFormat::Invalid {
                if let Some(format) =
                    params
                        .formats
                        .iter()
                        .find_map(|f| match sample_spec_from_format(f) {
                            Ok(ss) => Some(ss),
                            Err(e) => {
                                warn!("rejecting invalid format: {:#}", e);
                                None
                            }
                        })
                {
                    sample_spec = format;
                }
            }

            // Check if the client set any buffer attrs
            // to -1, which indicates that we should
            // set the value.
            let mut buffer_attr = params.buffer_attr;
            configure_buffer(&mut buffer_attr, &sample_spec);

            let target_length = buffer_attr.target_length;

            let flags = params.flags;
            let mut stream = PlaybackStream {
                state: StreamState::Prebuffering(buffer_attr.pre_buffering as u64),
                buffer_attr,
                buffer: PlaybackBuffer::new(sample_spec, CAPTURE_SPEC),
                requested_bytes: target_length as usize,
                played_bytes: 0,
                write_offset: 0,
                read_offset: 0,
            };

            // Returning a nonzero pre_buffering value always causes the stream
            // to start after prebuffering is complete, even if the client
            // requested otherwise.
            if buffer_attr.pre_buffering == 0 || flags.start_corked {
                stream.state = StreamState::Corked;
            }

            let channel = server.next_playback_channel_index;
            server.next_playback_channel_index += 1;

            client.playback_streams.insert(channel, stream);

            let reply = pulse::CreatePlaybackStreamReply {
                channel,
                stream_index: 500,
                sample_spec,
                channel_map: params.channel_map,
                buffer_attr,
                requested_bytes: target_length,
                sink_name: Some(SINK_NAME.into()),
                format: server.default_format_info.clone(),
                stream_latency: 10000, // TODO
                ..Default::default()
            };

            write_reply(&mut client.socket, seq, &reply, client.protocol_version)?;
            Ok(())
        }
        pulse::Command::DrainPlaybackStream(channel) => {
            if let Some(stream) = client.playback_streams.get_mut(&channel) {
                // The ack gets sent once we finish draining.
                stream.state = StreamState::Draining(seq);
            }

            Ok(())
        }
        pulse::Command::GetPlaybackLatency(pulse::LatencyParams { channel, now, .. }) => {
            if let Some(stream) = client.playback_streams.get_mut(&channel) {
                let reply = pulse::PlaybackLatency {
                    sink_usec: 10000,
                    source_usec: 0,
                    playing: matches!(stream.state, StreamState::Playing),
                    local_time: now,
                    remote_time: time::SystemTime::now(),
                    write_offset: stream.write_offset as i64,
                    read_offset: stream.read_offset as i64,
                    underrun_for: u64::MAX,
                    playing_for: stream.played_bytes,
                };

                write_reply(&mut client.socket, seq, &reply, client.protocol_version)?;
            }

            Ok(())
        }
        pulse::Command::UpdatePlaybackStreamProplist(_) => {
            pulse::write_ack_message(&mut client.socket, seq)?;
            Ok(())
        }
        pulse::Command::CorkPlaybackStream(params) => {
            if let Some(stream) = client.playback_streams.get_mut(&params.channel) {
                match stream.state {
                    StreamState::Corked if !params.cork => {
                        let needed = stream
                            .buffer_attr
                            .target_length
                            .saturating_sub(stream.buffer.len_bytes() as u32);

                        stream.state = if needed > 0 {
                            // Request bytes to fill the buffer.
                            trace!(
                                id = params.channel,
                                bytes_needed = needed,
                                "requesting buffer write"
                            );
                            pulse::write_command_message(
                                &mut client.socket,
                                u32::MAX,
                                pulse::Command::Request(pulse::Request {
                                    channel: params.channel,
                                    length: needed,
                                }),
                                client.protocol_version,
                            )?;

                            stream.requested_bytes = needed as usize;
                            StreamState::Prebuffering(needed as u64)
                        } else {
                            StreamState::Playing
                        };
                    }
                    StreamState::Playing if params.cork => {
                        stream.state = StreamState::Corked;
                    }
                    _ => (),
                }
            }

            pulse::write_ack_message(&mut client.socket, seq)?;
            Ok(())
        }
        pulse::Command::FlushPlaybackStream(channel) => {
            if let Some(stream) = client.playback_streams.get_mut(&channel) {
                stream.buffer.clear();
                stream.requested_bytes = 0;
                stream.played_bytes = 0;
                stream.read_offset = stream.write_offset;
            }

            pulse::write_ack_message(&mut client.socket, seq)?;
            Ok(())
        }
        pulse::Command::Extension(_) => {
            pulse::write_error(&mut client.socket, seq, pulse::PulseError::NoExtension)?;
            Ok(())
        }
        _ => {
            warn!("ignoring command {:?}", cmd.tag());
            pulse::write_error(&mut client.socket, seq, pulse::PulseError::NotImplemented)?;

            Ok(())
        }
    }
}

fn sample_spec_from_format(f: &pulse::FormatInfo) -> anyhow::Result<pulse::SampleSpec> {
    let format = f
        .props
        .get(pulse::Prop::FormatSampleFormat)
        .context("missing sample format")?;
    let rate = f
        .props
        .get(pulse::Prop::FormatRate)
        .context("missing sample rate")?;
    let channels = f
        .props
        .get(pulse::Prop::FormatChannels)
        .context("missing channel count")?;

    let format = match sanitize_prop_str(format)? {
        "s16le" => pulse::SampleFormat::S16Le,
        "s16be" => pulse::SampleFormat::S16Be,
        "u8" => pulse::SampleFormat::U8,
        "s32le" => pulse::SampleFormat::S32Le,
        "s32be" => pulse::SampleFormat::S32Be,
        "s24le" => pulse::SampleFormat::S24Le,
        "s24be" => pulse::SampleFormat::S24Be,
        "float32le" => pulse::SampleFormat::Float32Le,
        "float32be" => pulse::SampleFormat::Float32Be,
        _ => bail!("unsupported sample format: {:?}", format),
    };

    let rate = sanitize_prop_str(rate)?
        .parse()
        .context(format!("invalid sample rate: {:?}", rate))?;

    let channels = sanitize_prop_str(channels)?
        .parse()
        .context(format!("invalid channel count: {:?}", channels))?;

    Ok(pulse::SampleSpec {
        format,
        sample_rate: rate,
        channels,
    })
}

fn sanitize_prop_str(b: &[u8]) -> anyhow::Result<&str> {
    let s = CStr::from_bytes_with_nul(b).context("invalid string")?;
    let s = s.to_str().context("invalid utf-8")?;
    Ok(s.trim_matches('"'))
}

fn handle_stream_write(
    client: &mut Client,
    desc: pulse::Descriptor,
    payload: &[u8],
) -> anyhow::Result<()> {
    let stream = match client.playback_streams.get_mut(&desc.channel) {
        Some(v) => v,
        None => {
            bail!("invalid channel")
        }
    };

    let buffer_len = stream.buffer.len_bytes();
    trace!(
        id = desc.channel,
        ?stream.state,
        write_len = desc.length,
        current_len = buffer_len,
        future_len = buffer_len + desc.length as usize,
        "got stream write",
    );

    // We don't handle seeks yet.
    if desc.offset != 0 {
        bail!("seeking not supported")
    }

    // Check for overrun.
    let remaining = (stream.buffer_attr.max_length as usize).saturating_sub(buffer_len);
    let overflow = payload.len().saturating_sub(remaining);
    let payload = if overflow > 0 {
        pulse::write_command_message(
            &mut client.socket,
            u32::MAX,
            pulse::Command::Overflow(overflow as u32),
            client.protocol_version,
        )?;

        &payload[..remaining as usize]
    } else {
        payload
    };

    if let StreamState::Prebuffering(n) = stream.state {
        let needed = n.saturating_sub(payload.len() as u64);
        if needed > 0 {
            stream.state = StreamState::Prebuffering(needed)
        } else {
            debug!("starting playback for stream {}", desc.channel);
            pulse::write_command_message(
                &mut client.socket,
                u32::MAX,
                pulse::Command::Started(desc.channel),
                client.protocol_version,
            )?;

            stream.state = StreamState::Playing
        }
    }

    // Read the data into the buffer.
    stream.buffer.write(payload);
    stream.requested_bytes = stream.requested_bytes.saturating_sub(payload.len());
    stream.write_offset += payload.len() as u64;

    Ok(())
}

fn configure_buffer(attr: &mut pulse::stream::BufferAttr, spec: &pulse::SampleSpec) {
    let sample_size = spec.format.bytes_per_sample();
    let frame_size = spec.channels as usize * sample_size;
    let len_10ms = (frame_size * spec.sample_rate as usize / 100) as u32;

    // Max length is min(200ms, client value).
    if attr.max_length == u32::MAX {
        attr.max_length = len_10ms * 20;
    } else {
        attr.max_length = attr
            .max_length
            .next_multiple_of(frame_size as u32)
            .min(len_10ms * 100);
    }

    // Minimum request length is max(5ms, client value).
    if attr.minimum_request_length == u32::MAX {
        attr.minimum_request_length = (len_10ms / 2).next_multiple_of(frame_size as u32);
    } else {
        attr.minimum_request_length = attr
            .minimum_request_length
            .next_multiple_of(frame_size as u32)
            .max(len_10ms / 2);
    }

    // Target length should be a multiple of the minimum request length, and by
    // default 20ms of audio.
    if attr.target_length == u32::MAX {
        attr.target_length = (len_10ms * 2)
            .next_multiple_of(attr.minimum_request_length)
            .min(attr.max_length);
    } else {
        attr.target_length = attr
            .target_length
            .next_multiple_of(attr.minimum_request_length)
            .max(len_10ms)
            .min(attr.max_length);

        if attr.target_length < (attr.minimum_request_length * 2) {
            attr.target_length = attr.minimum_request_length * 2;
        }
    }

    // Prebuffering shouldn't be more than the target length.
    if attr.pre_buffering == u32::MAX {
        attr.pre_buffering = attr.target_length;
    } else {
        attr.pre_buffering = attr
            .pre_buffering
            .next_multiple_of(attr.minimum_request_length)
            .min(attr.target_length);
    }
}

fn write_reply<T: pulse::CommandReply + std::fmt::Debug>(
    socket: &mut mio::net::UnixStream,
    seq: u32,
    reply: &T,
    version: u16,
) -> anyhow::Result<()> {
    trace!("sending reply [{}] ({}): {:#?}", seq, version, reply);
    pulse::write_reply_message(socket, seq, reply, version)?;

    Ok(())
}
