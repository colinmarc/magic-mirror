// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::sync::Arc;

use anyhow::{anyhow, Context};
use ffmpeg_next as ffmpeg;
use ffmpeg_sys::avcodec_receive_packet;
use ffmpeg_sys_next as ffmpeg_sys;

use crate::{
    codec::VideoCodec,
    compositor::{video::timebase::Timebase, VideoStreamParams},
};

pub struct FFmpegPacket(Arc<ffmpeg::Packet>);

impl std::fmt::Debug for FFmpegPacket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FFmpegPacket")
            .field("size", &self.0.size())
            .field("pts", &self.0.pts())
            .field("dts", &self.0.dts())
            .finish()
    }
}

impl AsRef<[u8]> for FFmpegPacket {
    fn as_ref(&self) -> &[u8] {
        self.0.data().unwrap()
    }
}

pub struct FFmpegEncoder {
    enc: ffmpeg::encoder::Video,
    frame: ffmpeg::frame::Video,
    packet: Arc<ffmpeg::Packet>,
}

impl super::EncoderInner for FFmpegEncoder {
    type Packet = FFmpegPacket;

    fn send_picture(&mut self, frame: &super::VkExtMemoryFrame, pts: u64) -> anyhow::Result<()> {
        copy_frame(&frame, &mut self.frame);
        self.frame.set_pts(Some(pts.try_into()?));

        self.enc
            .send_frame(&self.frame)
            .context("error in send_frame")
    }

    fn receive_packet(&mut self) -> anyhow::Result<Option<Self::Packet>> {
        // ffmpeg::Encoder::receive_packet doesn't let us do our own reference
        // counting. It's also necessary to reset the packet metadata for each
        // NAL.
        unsafe {
            use ffmpeg::packet::Ref;
            ffmpeg_sys::av_init_packet(self.packet.as_ptr() as *mut _);

            match avcodec_receive_packet(self.enc.as_mut_ptr(), self.packet.as_ptr() as *mut _) {
                v if v == ffmpeg_sys::AVERROR_EOF || v == -ffmpeg_sys::EAGAIN => Ok(None),
                v if v < 0 => Err(anyhow!(ffmpeg::Error::from(v))),
                _ => Ok(Some(FFmpegPacket(self.packet.clone()))),
            }
        }
    }

    fn flush(&mut self) -> anyhow::Result<()> {
        self.enc.send_eof()?;
        Ok(())
    }
}

fn copy_frame(buf: &&super::VkExtMemoryFrame, frame: &mut ffmpeg::frame::Video) {
    // Copy from the staging buffer to the frame.
    for (plane, offset) in buf.offsets.iter().enumerate() {
        let len = frame.data(plane).len();
        let slice = unsafe {
            let ptr = buf.buffer.access as *const u8;
            let ptr = ptr.add(*offset);
            std::slice::from_raw_parts(ptr, len)
        };

        frame.data_mut(plane).copy_from_slice(slice);
    }
}

pub fn new_encoder(
    params: VideoStreamParams,
    framerate: u32,
    timebase: Timebase,
) -> anyhow::Result<FFmpegEncoder> {
    let codec_id =
        ffmpeg::encoder::find(params.codec.into()).ok_or(anyhow::anyhow!("codec not found"))?;

    let enc_ctx = unsafe {
        let ptr = ffmpeg_sys::avcodec_alloc_context3(codec_id.as_ptr());
        ffmpeg::codec::context::Context::wrap(ptr, None)
    };

    let mut encoder = enc_ctx.encoder().video().context("creating encoder")?;
    encoder.set_height(params.height);
    encoder.set_width(params.width);
    encoder.set_format(ffmpeg::format::pixel::Pixel::YUV420P);
    encoder.set_time_base(timebase);
    encoder.set_frame_rate(Some((framerate as i32, 1)));
    encoder.set_gop(120);
    encoder.set_quality(25);

    // This just tags the output - it doesn't actually perform any
    // conversion.
    // TODO: enforce this better.
    encoder.set_colorspace(ffmpeg::color::Space::BT709);
    encoder.set_color_range(ffmpeg::color::Range::MPEG);

    let mut opts = ffmpeg::Dictionary::new();
    opts.set("crf", "35");
    opts.set("repeat-headers", "1");
    opts.set("tune", "zerolatency");

    let enc = encoder.open_as_with(codec_id, opts)?;
    let frame =
        ffmpeg::frame::Video::new(ffmpeg::format::Pixel::YUV420P, params.width, params.height);
    let packet = Arc::new(ffmpeg::Packet::empty());

    Ok(FFmpegEncoder { enc, frame, packet })
}

pub fn probe_codec(codec: VideoCodec) -> bool {
    ffmpeg::encoder::find(codec.into()).is_some()
}
