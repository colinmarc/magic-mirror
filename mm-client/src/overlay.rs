// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

pub struct Overlay {
    message: Option<String>,
}

impl Overlay {
    pub fn new() -> Self {
        Self { message: None }
    }

    pub fn set_message(&mut self, s: &str) {
        self.message = Some(s.to_owned());
    }

    pub fn clear_message(&mut self) {
        self.message = None;
    }

    pub fn build(&mut self, ui: &imgui::Ui) -> anyhow::Result<()> {
        if self.message.is_none() {
            return Ok(());
        }

        let _style = ui.push_style_var(imgui::StyleVar::WindowBorderSize(0.0));

        let [_width, height] = ui.io().display_size;

        if let Some(_window) = ui
            .window("overlay")
            .position([0.0, height], imgui::Condition::Always)
            .position_pivot([0.0, 1.0])
            .no_decoration()
            .no_nav()
            .movable(false)
            .always_auto_resize(true)
            .bg_alpha(0.5)
            .begin()
        {
            ui.set_window_font_scale(2.0);
            ui.text(self.message.as_ref().unwrap());
        }

        Ok(())
    }
}

impl Default for Overlay {
    fn default() -> Self {
        Self::new()
    }
}
