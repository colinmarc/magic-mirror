// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use svt::Packet;

use crate::compositor::VideoStreamParams;

pub struct SvtEncoder<T: svt::Encoder> {
    inner: T,
    flushing: bool,
    done: bool,
}

impl<T: svt::Encoder> super::EncoderInner for SvtEncoder<T> {
    type Packet = T::Packet;

    fn send_picture(&mut self, frame: &super::VkExtMemoryFrame, pts: u64) -> anyhow::Result<()> {
        self.inner.send_picture(frame, pts.try_into()?, false)?;
        Ok(())
    }

    fn receive_packet(&mut self) -> anyhow::Result<Option<Self::Packet>> {
        if self.done {
            return Ok(None);
        }

        match self.inner.get_packet(self.flushing)? {
            Some(packet) => {
                if packet.is_eos() {
                    self.done = true;
                }

                Ok(Some(packet))
            }
            None => Ok(None),
        }
    }

    fn flush(&mut self) -> anyhow::Result<()> {
        self.flushing = true;
        self.inner.finish()?;
        Ok(())
    }
}

impl svt::Picture for super::VkExtMemoryFrame {
    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn stride(&self, plane: svt::Plane) -> u32 {
        self.strides[plane as usize] as u32
    }

    fn as_slice(&self, plane: svt::Plane) -> &[u8] {
        // We always use 4:2:0.
        let offset = self.offsets[plane as usize];
        let stride = self.strides[plane as usize];
        let len = match plane {
            svt::Plane::Y => stride * self.height as usize,
            svt::Plane::U | svt::Plane::V => stride * (self.height as usize / 2),
        };

        unsafe {
            let ptr = self.buffer.access as *const u8;
            let ptr = ptr.add(offset);
            std::slice::from_raw_parts(ptr, len)
        }
    }
}

pub fn new_hevc(
    params: VideoStreamParams,
    framerate: u32,
) -> Result<SvtEncoder<svt::hevc::HevcEncoder>, svt::Error> {
    let enc = svt::hevc::HevcEncoderConfig::default()
        .framerate(framerate, 1)
        .look_ahead_distance(1)
        .code_eos(true)
        .code_vps_sps_pps(true)
        .enable_fps_in_vps(true)
        .pred_structure(svt::hevc::PredictionStructure::LowDelayP)
        .rate_control_mode(svt::hevc::RateControlMode::ConstantQp)
        .qp(17)
        .intra_refresh_type(svt::hevc::IntraRefreshType::Closed(300))
        .thread_count(1)
        .create_encoder(params.width, params.height, svt::SubsamplingFormat::Yuv420)?;

    Ok(SvtEncoder {
        inner: enc,
        flushing: false,
        done: false,
    })
}

pub fn new_av1(
    params: VideoStreamParams,
    framerate: u32,
) -> Result<SvtEncoder<svt::av1::Av1Encoder>, svt::Error> {
    let enc = svt::av1::Av1EncoderConfig::default()
        .preset(10)
        .framerate(framerate, 1)
        .look_ahead_distance(1)
        .enable_screen_content_mode(true)
        .enable_fast_decode(true)
        .pred_structure(svt::av1::PredictionStructure::LowDelay)
        .rate_control_mode(svt::av1::RateControlMode::ConstantRateFactor(17))
        .intra_refresh_type(svt::av1::IntraRefreshType::Closed)
        .create_encoder(params.width, params.height, svt::SubsamplingFormat::Yuv420)?;

    Ok(SvtEncoder {
        inner: enc,
        flushing: false,
        done: false,
    })
}
