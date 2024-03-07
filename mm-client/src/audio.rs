// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use anyhow::{bail, Context as _};
use bytes::Buf as _;
use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait};
use crossbeam_channel as crossbeam;
use tracing::{debug, error, info, trace};

use crate::packet_ring::{self, PacketRing};
use mm_protocol as protocol;

trait DecodePacket<T: Send> {
    fn decode(&mut self, input: &[u8], output: &mut [T]) -> anyhow::Result<usize>;
}

impl DecodePacket<f32> for opus::Decoder {
    fn decode(&mut self, packet: &[u8], output: &mut [f32]) -> anyhow::Result<usize> {
        let len = self.decode_float(packet, output, false)?;
        Ok(len)
    }
}

impl DecodePacket<i16> for opus::Decoder {
    fn decode(&mut self, packet: &[u8], output: &mut [i16]) -> anyhow::Result<usize> {
        let len = self.decode(packet, output, false)?;
        Ok(len)
    }
}

enum DecoderType {
    F32(Decoder<f32>),
    I16(Decoder<i16>),
}

struct Decoder<T> {
    buffer: Arc<Mutex<VecDeque<T>>>,
    thread_handle: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
    undecoded_tx: Option<crossbeam::Sender<packet_ring::Packet>>,
}

impl<T> Decoder<T>
where
    T: cpal::SizedSample + Default + Send + 'static,
    opus::Decoder: DecodePacket<T>,
{
    pub fn new(channels: u32, sample_rate: u32) -> anyhow::Result<Self> {
        let ch = match channels {
            1 => opus::Channels::Mono,
            2 => opus::Channels::Stereo,
            _ => bail!("unsupported number of channels: {}", channels),
        };

        let mut decoder = opus::Decoder::new(sample_rate, ch)?;

        let buffer: Arc<Mutex<VecDeque<T>>> = Arc::new(Mutex::new(VecDeque::new()));

        let buffer_clone = buffer.clone();
        let (undecoded_tx, undecoded_recv) = crossbeam::unbounded::<packet_ring::Packet>();

        let thread_handle = std::thread::Builder::new()
            .name("audio decode".into())
            .spawn(move || {
                let mut output = vec![Default::default(); (sample_rate * channels / 100) as usize];

                loop {
                    let mut packet = match undecoded_recv.recv() {
                        Ok(packet) => packet,
                        Err(crossbeam::RecvError) => return Ok(()),
                    };

                    let packet = packet.copy_to_bytes(packet.remaining());
                    match DecodePacket::decode(&mut decoder, &packet, &mut output) {
                        Ok(len) => {
                            if len == 0 {
                                continue;
                            }

                            buffer_clone
                                .lock()
                                .unwrap()
                                .extend(output[..(len * channels as usize)].iter());
                        }
                        Err(e) => {
                            error!("opus decode error: {}", e);
                            continue;
                        }
                    };
                }
            })?;

        Ok(Self {
            buffer,
            thread_handle: Some(thread_handle),
            undecoded_tx: Some(undecoded_tx),
        })
    }
}

impl<T> Drop for Decoder<T> {
    fn drop(&mut self) {
        let _ = self.undecoded_tx.take();
        if let Some(handle) = self.thread_handle.take() {
            match handle.join() {
                Ok(Ok(())) => (),
                Ok(Err(e)) => {
                    error!("audio decode thread error: {}", e);
                }
                Err(_) => {
                    error!("audio decode thread panicked");
                }
            }
        }
    }
}

pub struct AudioStream {
    device: cpal::Device,

    stream: Option<cpal::Stream>,
    decoder: Option<DecoderType>,
    stream_waiting: bool,

    ring: PacketRing,
    stream_seq: u64,
    packet_count: u64,
}

impl AudioStream {
    pub fn new() -> anyhow::Result<Self> {
        let device = cpal::default_host()
            .default_output_device()
            .context("unable to find default audio output device")?;

        info!("using audio output device: {}", device.name()?);

        Ok(Self {
            device,

            stream: None,
            decoder: None,
            stream_waiting: true,
            packet_count: 0,

            ring: PacketRing::new(),
            stream_seq: 0,
        })
    }

    pub fn reset(
        &mut self,
        stream_seq: u64,
        sample_rate: u32,
        channels: u32,
    ) -> anyhow::Result<()> {
        debug!(
            stream_seq,
            sample_rate, channels, "starting or restarting audio stream"
        );

        let (format, conf) = select_conf(&self.device, sample_rate, channels)?;

        let (dec, stream) = match format {
            cpal::SampleFormat::F32 => {
                let dec = Decoder::new(channels, sample_rate)?;
                let stream = build_stream::<f32>(&self.device, conf, &dec)?;

                (DecoderType::F32(dec), stream)
            }
            cpal::SampleFormat::I16 => {
                let dec = Decoder::new(channels, sample_rate)?;
                let stream = build_stream::<i16>(&self.device, conf, &dec)?;

                (DecoderType::I16(dec), stream)
            }
            _ => unreachable!(),
        };

        self.stream_seq = stream_seq;
        self.stream = Some(stream);
        self.decoder = Some(dec);
        self.stream_waiting = true;
        self.packet_count = 0;

        Ok(())
    }

    pub fn recv_chunk(&mut self, chunk: protocol::AudioChunk) -> anyhow::Result<()> {
        self.ring.recv_chunk(chunk)?;

        let dec = match &mut self.decoder {
            Some(dec) => dec,
            None => return Ok(()),
        };

        let undecoded_tx = match &dec {
            DecoderType::F32(dec) => dec.undecoded_tx.as_ref().unwrap(),
            DecoderType::I16(dec) => dec.undecoded_tx.as_ref().unwrap(),
        };

        for packet in self.ring.drain_completed(self.stream_seq) {
            trace!(
                stream_seq = self.stream_seq,
                seq = packet.seq,
                len = bytes::Buf::remaining(&packet),
                "received full audio packet"
            );

            self.packet_count += 1;
            undecoded_tx.send(packet)?;
        }

        if self.stream.is_some() && self.stream_waiting && self.packet_count > 2 {
            self.stream_waiting = false;
            self.stream.as_ref().unwrap().play()?;
        }

        Ok(())
    }
}

fn build_stream<T: cpal::SizedSample + Default + Send + 'static>(
    device: &cpal::Device,
    conf: cpal::StreamConfig,
    dec: &Decoder<T>,
) -> anyhow::Result<cpal::Stream> {
    debug!("using audio output configuration: {:?}", conf);

    let buffer = dec.buffer.clone();
    let stream = device.build_output_stream(
        &conf,
        move |out, _info| {
            let mut buffer = buffer.lock().unwrap();
            if buffer.len() < out.len() {
                out.fill(Default::default());
                error!("audio buffer underrun");
                return;
            }

            for sample in out.iter_mut() {
                *sample = buffer.pop_front().unwrap();
            }
        },
        move |err| {
            error!("audio playback error: {}", err);
        },
        None,
    )?;

    Ok(stream)
}

fn select_conf(
    device: &cpal::Device,
    sample_rate: u32,
    channels: u32,
) -> anyhow::Result<(cpal::SampleFormat, cpal::StreamConfig)> {
    let mut confs = device
        .supported_output_configs()
        .context("unable to query supported audio playback formats")?;

    let valid = |format: cpal::SampleFormat| {
        move |conf: &cpal::SupportedStreamConfigRange| {
            conf.sample_format() == format
                && conf.min_sample_rate() <= cpal::SampleRate(sample_rate)
                && conf.max_sample_rate() >= cpal::SampleRate(sample_rate)
                && conf.channels() == channels as u16
        }
    };

    if let Some(conf_range) = confs
        .find(valid(cpal::SampleFormat::F32))
        .or_else(|| confs.find(valid(cpal::SampleFormat::I16)))
    {
        let sample_format = conf_range.sample_format();
        let buffer_size = match conf_range.buffer_size() {
            cpal::SupportedBufferSize::Unknown => cpal::BufferSize::Default,
            cpal::SupportedBufferSize::Range { min, .. } => {
                cpal::BufferSize::Fixed(std::cmp::max(*min, sample_rate / 100))
            }
        };

        let mut conf =
            cpal::StreamConfig::from(conf_range.with_sample_rate(cpal::SampleRate(sample_rate)));
        conf.buffer_size = buffer_size;

        return Ok((sample_format, conf));
    }

    bail!("no valid audio output configuration found");
}
