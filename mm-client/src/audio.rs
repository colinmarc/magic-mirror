// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

mod buffer;

use std::{
    sync::{Arc, Mutex},
    time,
};

use anyhow::{bail, Context as _};
use buffer::PlaybackBuffer;
use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait};
use crossbeam_channel as crossbeam;
use dasp::Signal;
use mm_client_common as client;
use tracing::{debug, error, info, trace};

trait DecodePacket<T> {
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

// This is a trait object so we can erase the sample/frame generic type.
trait StreamWrapper {
    #[allow(clippy::new_ret_no_self)]
    fn new(
        device: &cpal::Device,
        conf: cpal::StreamConfig,
    ) -> anyhow::Result<(Box<dyn StreamWrapper>, cpal::Stream)>
    where
        Self: Sized;

    fn sync(&mut self, pts: u64);
    fn send_packet(&mut self, packet: Arc<client::Packet>) -> anyhow::Result<()>;
}

struct StreamInner<F: dasp::Frame> {
    sync_point: Arc<Mutex<Option<(u64, time::Instant)>>>,
    _buffer: Arc<Mutex<PlaybackBuffer<F>>>,
    thread_handle: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
    undecoded_tx: Option<crossbeam::Sender<Arc<client::Packet>>>,
}

impl<F> StreamWrapper for StreamInner<F>
where
    F: dasp::Frame + Send + 'static,
    F::Sample: cpal::SizedSample + dasp::sample::Duplex<f64> + Default,
    opus::Decoder: DecodePacket<F::Sample>,
    for<'a> &'a [F::Sample]: dasp::slice::ToFrameSlice<'a, F>,
{
    fn new(
        device: &cpal::Device,
        conf: cpal::StreamConfig,
    ) -> anyhow::Result<(Box<dyn StreamWrapper>, cpal::Stream)> {
        let sample_rate = conf.sample_rate.0;

        let mut decoder = {
            let ch = match F::CHANNELS {
                1 => opus::Channels::Mono,
                2 => opus::Channels::Stereo,
                _ => bail!("unsupported number of channels: {}", F::CHANNELS),
            };

            opus::Decoder::new(sample_rate, ch)?
        };

        let buffer = Arc::new(Mutex::new(PlaybackBuffer::new()));
        let (undecoded_tx, undecoded_recv) = crossbeam::unbounded::<Arc<client::Packet>>();

        // Spawn a thread to eagerly decode packets.
        let buffer_clone = buffer.clone();
        let thread_handle = std::thread::Builder::new()
            .name("audio decode".into())
            .spawn(move || {
                // Handles up to 100ms of decoded audio.
                let mut output =
                    vec![Default::default(); (sample_rate * F::CHANNELS as u32 / 10) as usize];

                loop {
                    let packet = match undecoded_recv.recv() {
                        Ok(packet) => packet,
                        Err(crossbeam::RecvError) => return Ok(()),
                    };

                    let pts = packet.pts();
                    let packet = packet.data();
                    match DecodePacket::decode(&mut decoder, &packet, &mut output) {
                        Ok(len) => {
                            if len == 0 {
                                continue;
                            }

                            let frames =
                                dasp::slice::to_frame_slice(&output[..(len * F::CHANNELS)])
                                    .expect("invalid sample count");

                            let mut guard = buffer_clone.lock().unwrap();
                            guard.buffer(pts, frames);

                            #[cfg(feature = "tracy")]
                            {
                                let len_us = guard.len() as f64 / sample_rate as f64 * 1_000_000.0;
                                tracy_client::plot!("audio buffer (μs)", len_us);
                            }
                        }
                        Err(e) => {
                            error!("opus decode error: {}", e);
                            continue;
                        }
                    };
                }
            })?;

        // The current PTS of the video stream, which we want to sync to.
        let sync_point = Arc::new(Mutex::new(None));

        let sync_point_clone = sync_point.clone();
        let buffer_clone = buffer.clone();
        let stream = device.build_output_stream(
            &conf,
            move |out, _info| {
                let mut buffer = buffer_clone.lock().unwrap();

                let frames_needed = out.len() / F::CHANNELS;
                let frames_remaining = buffer.len(); // In frames.

                let frames_per_ms = sample_rate / 1000;

                if frames_remaining < frames_needed {
                    out.fill(Default::default());

                    trace!("audio buffer underrun");
                    return;
                }

                let sync_point: Option<(u64, time::Instant)> =
                    sync_point_clone.lock().unwrap().as_ref().copied();
                if let Some((pts, ts)) = sync_point {
                    let target_pts = pts + ts.elapsed().as_millis() as u64;
                    let pts = buffer.current_pts();

                    let delay = target_pts as i64 - pts as i64;

                    #[cfg(feature = "tracy")]
                    tracy_client::plot!("audio drift (ms)", delay as f64);

                    // Outside these bounds, skip or play silence in order to sync.
                    const TOO_EARLY: i64 = 20;
                    const TOO_LATE: i64 = 60;

                    if delay < TOO_EARLY {
                        // Play silence until the video catches up.
                        out.fill(Default::default());
                        return;
                    }

                    if delay > TOO_LATE {
                        // Skip ahead.
                        let skip = std::cmp::min(
                            (delay * frames_per_ms as i64) as usize,
                            frames_remaining.saturating_sub(frames_needed * 2),
                        );

                        buffer.skip(skip);
                    }
                }

                let mut signal = dasp::signal::from_iter(buffer.drain()).into_interleaved_samples();

                for sample in out.iter_mut() {
                    *sample = signal.next_sample();
                }

                #[cfg(feature = "tracy")]
                {
                    let len_us = buffer.len() as f64 / sample_rate as f64 * 1_000_000.0;
                    tracy_client::plot!("audio buffer (μs)", len_us);
                }
            },
            move |err| {
                error!("audio playback error: {}", err);
            },
            None,
        )?;

        Ok((
            Box::new(Self {
                // decoded_packets,
                _buffer: buffer,
                sync_point,
                thread_handle: Some(thread_handle),
                undecoded_tx: Some(undecoded_tx),
            }),
            stream,
        ))
    }

    fn sync(&mut self, pts: u64) {
        *self.sync_point.lock().unwrap() = Some((pts, time::Instant::now()));
    }

    fn send_packet(&mut self, packet: Arc<client::Packet>) -> anyhow::Result<()> {
        self.undecoded_tx
            .as_ref()
            .unwrap()
            .send(packet)
            .map_err(|_| anyhow::anyhow!("audio decode thread died"))?;
        Ok(())
    }
}

impl<T: dasp::Frame> Drop for StreamInner<T> {
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
    inner: Option<Box<dyn StreamWrapper>>,

    stream_waiting: bool,

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
            inner: None,
            stream_waiting: true,
            packet_count: 0,

            stream_seq: 0,
        })
    }

    pub fn sync(&mut self, pts: u64) {
        if let Some(inner) = &mut self.inner {
            inner.sync(pts);
        }
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

        let (inner, stream) = match (format, channels) {
            (cpal::SampleFormat::F32, 1) => StreamInner::<[f32; 1]>::new(&self.device, conf),
            (cpal::SampleFormat::F32, 2) => StreamInner::<[f32; 2]>::new(&self.device, conf),
            (cpal::SampleFormat::I16, 1) => StreamInner::<[i16; 1]>::new(&self.device, conf),
            (cpal::SampleFormat::I16, 2) => StreamInner::<[i16; 2]>::new(&self.device, conf),
            _ => bail!("unsupported sample rate / format"),
        }?;

        self.stream_seq = stream_seq;
        self.stream = Some(stream);
        self.inner = Some(inner);
        self.stream_waiting = true;
        self.packet_count = 0;

        Ok(())
    }

    pub fn recv_packet(&mut self, packet: Arc<client::Packet>) -> anyhow::Result<()> {
        if let Some(inner) = &mut self.inner {
            trace!(
                stream_seq = packet.stream_seq(),
                seq = packet.seq(),
                pts = packet.pts(),
                len = packet.len(),
                "received full audio packet"
            );

            self.packet_count += 1;
            inner.send_packet(packet)?;
        }

        if self.stream.is_some() && self.stream_waiting && self.packet_count > 2 {
            self.stream_waiting = false;
            self.stream.as_ref().unwrap().play()?;
        }

        Ok(())
    }
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
                let size = std::cmp::max(*min, sample_rate / 100);
                let mut buffer_size = 2;
                while buffer_size < size {
                    buffer_size *= 2;
                }
                cpal::BufferSize::Fixed(buffer_size)
            }
        };

        let mut conf =
            cpal::StreamConfig::from(conf_range.with_sample_rate(cpal::SampleRate(sample_rate)));
        conf.buffer_size = buffer_size;

        return Ok((sample_format, conf));
    }

    bail!("no valid audio output configuration found");
}
