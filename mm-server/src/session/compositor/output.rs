// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use wayland_server::{protocol::wl_output, Resource as _};

use crate::session::compositor::{Compositor, DisplayParams};

impl Compositor {
    pub fn emit_output_params(&mut self) {
        let params = self.display_params;
        for proxy in &self.output_proxies {
            configure_output(proxy, params);
        }
    }
}

pub fn configure_output(output: &wl_output::WlOutput, params: DisplayParams) {
    let version = output.version();
    if version >= 4 {
        output.name("MM".to_string());
        output.description("Magic Mirror Virtual Display".to_string());
    }

    output.geometry(
        0,
        0,
        params.width as i32,
        params.height as i32,
        wl_output::Subpixel::None,
        "Magic Mirror".to_string(),
        "Virtual Display".to_string(),
        wl_output::Transform::Normal,
    );

    output.mode(
        wl_output::Mode::Current | wl_output::Mode::Preferred,
        params.width as i32,
        params.height as i32,
        params.framerate as i32 * 1000,
    );

    if version >= 2 {
        // In the case of fractional scale, we always send the next integer
        // (and then scale down for clients that don't support fractional scale).
        let scale: f64 = params.ui_scale.into();
        output.scale(scale.ceil() as i32);

        output.done();
    }
}
