// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use std::time;

const FLASH_DURATION: time::Duration = time::Duration::from_millis(1350);
const FADE_OUT_AFTER: time::Duration = time::Duration::from_millis(1000);

pub struct Flash {
    message: Option<(String, time::Instant)>,
}

impl Flash {
    pub fn new() -> Self {
        Self { message: None }
    }

    pub fn set_message(&mut self, s: &str) {
        self.message = Some((s.to_owned(), time::Instant::now()));
    }

    pub fn build(&mut self, ui: &imgui::Ui) -> anyhow::Result<()> {
        if self.message.is_none() {
            return Ok(());
        }

        let start = self.message.as_ref().unwrap().1;
        if start.elapsed() > FLASH_DURATION {
            self.message = None;
            return Ok(());
        }

        let alpha = if start.elapsed() > FADE_OUT_AFTER {
            let remaining = FLASH_DURATION - start.elapsed();
            remaining.as_secs_f32() / (FLASH_DURATION - FADE_OUT_AFTER).as_secs_f32()
        } else {
            1.0
        };

        // Exponentially ease the alpha.
        let alpha = alpha * alpha;

        let _style_alpha = ui.push_style_var(imgui::StyleVar::Alpha(alpha));
        let _style_border = ui.push_style_var(imgui::StyleVar::WindowBorderSize(0.0));

        let [_width, height] = ui.io().display_size;

        if let Some(_window) = ui
            .window("flash")
            .position([0.0, height], imgui::Condition::Always)
            .position_pivot([0.0, 1.0])
            .no_decoration()
            .no_nav()
            .movable(false)
            .always_auto_resize(true)
            .bg_alpha(0.5 * alpha)
            .begin()
        {
            ui.set_window_font_scale(2.0);
            ui.text(&self.message.as_ref().unwrap().0);
        }

        Ok(())
    }
}

impl Default for Flash {
    fn default() -> Self {
        Self::new()
    }
}
