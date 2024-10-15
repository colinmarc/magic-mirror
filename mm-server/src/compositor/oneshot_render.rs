// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use bytes::Bytes;
use drm_fourcc::DrmFourcc;
use image::ImageEncoder as _;
use tracing::instrument;

use crate::compositor::buffers::{fourcc_bpp, PlaneMetadata};
use crate::vulkan::VkHostBuffer;

#[instrument(skip_all)]
pub fn shm_to_png(buffer: &VkHostBuffer, format: PlaneMetadata) -> anyhow::Result<Bytes> {
    // Needs to be updated if we start supporting float shm buffers.
    match fourcc_bpp(format.format) {
        Some(4) => (),
        _ => panic!("shm texture has unexpected format"),
    }

    let src = unsafe {
        std::slice::from_raw_parts_mut(
            buffer.access as *mut u8,
            (format.stride * format.height) as usize,
        )
    };

    let mut buf = vec![0_u8; (format.stride * format.height) as usize];
    buf.copy_from_slice(src);

    let width = format.width as usize;
    let height = format.height as usize;
    let format = format.format;

    // For png, we need rgba8 with no padding.
    let mut out = Vec::with_capacity(width * height * 4);
    match format {
        DrmFourcc::Argb8888 | DrmFourcc::Xrgb8888 => {
            for row in buf.chunks_exact(width * 4) {
                for px in row.chunks_exact(4) {
                    let out_px = [px[2], px[1], px[0], px[3]];
                    out.extend_from_slice(&out_px);
                }
            }
        }
        _ => unreachable!(),
    }

    let mut png = std::io::Cursor::new(Vec::new());
    image::codecs::png::PngEncoder::new(&mut png).write_image(
        &out,
        width as u32,
        height as u32,
        image::ExtendedColorType::Rgba8,
    )?;

    Ok(png.into_inner().into())
}
