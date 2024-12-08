// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

mod wl_buffer;
mod wl_compositor;
mod wl_data_device_manager;
mod wl_drm;
mod wl_output;
mod wl_seat;
mod wl_shm;
mod wp_fractional_scale;
mod wp_linux_dmabuf;
mod wp_pointer_constraints;
mod wp_presentation;
mod wp_relative_pointer;
mod wp_text_input;
mod xdg_shell;
mod xwayland_shell;

fn make_u64(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}
