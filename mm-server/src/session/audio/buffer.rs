// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{collections::VecDeque, io};

use byteorder::{BigEndian as BE, LittleEndian as LE, ReadBytesExt as _};
use dasp::{interpolate::sinc::Sinc, ring_buffer, signal::interpolate::Converter};
use pulseaudio::protocol as pulse;

/// Raw bytes go in, (optionally) resampled frames come out.
pub enum PlaybackBuffer<F>
where
    F: dasp::Frame<Sample = f32>,
{
    Passthrough(Buffer<F>),
    Resampling {
        converter: Converter<Buffer<F>, Sinc<[F; 32]>>,
        output_rate: u32,
    },
}

impl<F> PlaybackBuffer<F>
where
    F: dasp::Frame<Sample = f32>,
{
    pub fn new(sample_spec: pulse::SampleSpec, output_spec: pulse::SampleSpec) -> Self {
        // TODO: downmixing
        assert_eq!(
            sample_spec.channels, output_spec.channels,
            "down- or upmixing is not supported"
        );

        let buffer = Buffer::new(sample_spec);
        if sample_spec.sample_rate == output_spec.sample_rate {
            Self::Passthrough(buffer)
        } else {
            let ringbuf = ring_buffer::Fixed::from([F::EQUILIBRIUM; 32]);
            let interpolator = Sinc::new(ringbuf);
            Self::Resampling {
                converter: dasp::Signal::from_hz_to_hz(
                    buffer,
                    interpolator,
                    sample_spec.sample_rate as f64,
                    output_spec.sample_rate as f64,
                ),
                output_rate: output_spec.sample_rate,
            }
        }
    }

    fn buffer(&self) -> &Buffer<F> {
        match self {
            PlaybackBuffer::Passthrough(ref buffer) => buffer,
            PlaybackBuffer::Resampling { converter, .. } => converter.source(),
        }
    }

    fn buffer_mut(&mut self) -> &mut Buffer<F> {
        match self {
            PlaybackBuffer::Passthrough(ref mut buffer) => buffer,
            PlaybackBuffer::Resampling { converter, .. } => converter.source_mut(),
        }
    }

    pub fn len_bytes(&self) -> usize {
        self.buffer().len_bytes()
    }

    pub fn len_frames(&self) -> usize {
        self.buffer().len_frames()
    }

    pub fn is_empty(&self) -> bool {
        self.len_frames() == 0
    }

    pub fn write(&mut self, payload: &[u8]) {
        let _ = io::Write::write_all(&mut self.buffer_mut().inner, payload);
    }

    /// Reads data from the buffer at the output sample rate, returning
    /// `num_frames` at that rate, or None if there's insufficient data.
    ///
    /// Dropping the returned signal removes the remaining unread data.
    pub fn drain(&mut self, num_frames: usize) -> Option<impl dasp::Signal<Frame = F> + '_> {
        match self {
            PlaybackBuffer::Passthrough(buffer) => buffer.drain(num_frames).map(EitherSignal::Left),
            PlaybackBuffer::Resampling {
                ref mut converter,
                output_rate,
            } => {
                let buffer = converter.source();
                let needed_frames = (buffer.sample_spec.sample_rate as usize * num_frames)
                    .div_ceil(*output_rate as usize);

                if buffer.len_frames() < needed_frames {
                    return None;
                }

                Some(EitherSignal::Right(Drain {
                    signal: converter,
                    remaining: num_frames,
                }))
            }
        }
    }

    pub fn clear(&mut self) {
        self.buffer_mut().inner.clear()
    }
}

enum EitherSignal<L, R> {
    Left(L),
    Right(R),
}

impl<L, R> dasp::Signal for EitherSignal<L, R>
where
    L: dasp::Signal,
    R: dasp::Signal<Frame = L::Frame>,
{
    type Frame = L::Frame;

    fn next(&mut self) -> Self::Frame {
        match self {
            EitherSignal::Left(s) => s.next(),
            EitherSignal::Right(s) => s.next(),
        }
    }

    fn is_exhausted(&self) -> bool {
        match self {
            EitherSignal::Left(s) => s.is_exhausted(),
            EitherSignal::Right(s) => s.is_exhausted(),
        }
    }
}

pub struct Buffer<F>
where
    F: dasp::Frame<Sample = f32>,
{
    inner: VecDeque<u8>,
    sample_spec: pulse::SampleSpec,
    bpp: usize,
    _phantom: std::marker::PhantomData<F>,
}

impl<F> Buffer<F>
where
    F: dasp::Frame<Sample = f32>,
{
    pub fn new(sample_spec: pulse::SampleSpec) -> Self {
        assert_eq!(sample_spec.channels as usize, F::CHANNELS);

        Self {
            inner: VecDeque::new(),
            sample_spec,
            bpp: sample_spec.format.bytes_per_sample(),
            _phantom: std::marker::PhantomData,
        }
    }

    fn len_bytes(&self) -> usize {
        self.inner.len()
    }

    fn len_frames(&self) -> usize {
        let input_channels = self.sample_spec.channels as usize;
        self.inner.len() / (input_channels * self.bpp)
    }

    fn read_frame(&mut self) -> Option<F> {
        if self.len_frames() == 0 {
            return None;
        }

        Some(F::from_fn(|_| self.read_sample().unwrap()))
    }

    fn read_sample(&mut self) -> Option<F::Sample> {
        use dasp::Sample;

        match self.sample_spec.format {
            pulse::SampleFormat::Float32Le => self.inner.read_f32::<LE>().ok(),
            pulse::SampleFormat::Float32Be => self.inner.read_f32::<BE>().ok(),
            pulse::SampleFormat::S16Le => self.inner.read_i16::<LE>().ok().map(Sample::from_sample),
            pulse::SampleFormat::S16Be => self.inner.read_i16::<BE>().ok().map(Sample::from_sample),
            pulse::SampleFormat::U8 => self.inner.read_u8().ok().map(Sample::from_sample),
            pulse::SampleFormat::S32Le => self.inner.read_i32::<LE>().ok().map(Sample::from_sample),
            pulse::SampleFormat::S32Be => self.inner.read_i32::<BE>().ok().map(Sample::from_sample),
            pulse::SampleFormat::S24Le => self.inner.read_i24::<LE>().ok().map(Sample::from_sample),
            _ => unimplemented!(),
        }
    }
}

impl<F> Buffer<F>
where
    F: dasp::Frame<Sample = f32>,
{
    fn drain(&mut self, num_frames: usize) -> Option<Drain<Self>> {
        if self.len_frames() < num_frames {
            return None; // Not enough data.
        }

        Some(Drain {
            signal: self,
            remaining: num_frames,
        })
    }
}

impl<F> dasp::Signal for Buffer<F>
where
    F: dasp::Frame<Sample = f32>,
{
    type Frame = F;

    fn next(&mut self) -> Self::Frame {
        self.read_frame()
            .unwrap_or(<Self::Frame as dasp::Frame>::EQUILIBRIUM)
    }
}

struct Drain<'a, S: dasp::Signal> {
    signal: &'a mut S,
    remaining: usize,
}

impl<S: dasp::Signal> dasp::Signal for Drain<'_, S> {
    type Frame = S::Frame;

    fn is_exhausted(&self) -> bool {
        self.remaining == 0
    }

    fn next(&mut self) -> Self::Frame {
        if self.remaining == 0 {
            <Self::Frame as dasp::Frame>::EQUILIBRIUM
        } else {
            self.remaining -= 1;
            dasp::Signal::next(&mut self.signal)
        }
    }
}

impl<S: dasp::Signal> Drop for Drain<'_, S> {
    fn drop(&mut self) {
        for _ in 0..self.remaining {
            if self.signal.is_exhausted() {
                break;
            }

            let _ = dasp::Signal::next(&mut self.signal);
        }
    }
}
