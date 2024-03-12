// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use std::{collections::VecDeque, vec};

use mm_protocol as protocol;

use crate::stats::STATS;

pub struct Overlay {
    streaming_width: u32,
    streaming_height: u32,
    codec: protocol::VideoCodec,

    video_latency_measurements: VecDeque<f32>,

    reposition: bool,
}

impl Overlay {
    pub fn new(fps: u32) -> Self {
        Self {
            streaming_width: 0,
            streaming_height: 0,
            codec: protocol::VideoCodec::H264,

            video_latency_measurements: VecDeque::from(vec![0.0; 10 * fps as usize]),

            reposition: true,
        }
    }

    pub fn reposition(&mut self) {
        self.reposition = true;
    }

    pub fn update_params(&mut self, params: &protocol::Attached) {
        self.streaming_width = params.streaming_resolution.as_ref().unwrap().width;
        self.streaming_height = params.streaming_resolution.as_ref().unwrap().height;
        self.codec = params.video_codec();
    }

    pub fn build(&mut self, ui: &imgui::Ui) -> anyhow::Result<()> {
        // Record a latency measurement.
        let latency = STATS.video_latency();
        self.video_latency_measurements.rotate_left(1);
        *self.video_latency_measurements.back_mut().unwrap() = latency;

        let [width, height] = ui.io().display_size;
        let [scale_x, scale_y] = ui.io().display_framebuffer_scale;

        let condition = if self.reposition {
            self.reposition = false;
            imgui::Condition::Always
        } else {
            imgui::Condition::Once
        };

        let _padding = ui.push_style_var(imgui::StyleVar::WindowPadding([8.0, 8.0]));
        let _rounding = ui.push_style_var(imgui::StyleVar::WindowRounding(4.0));
        let _frame_rounding = ui.push_style_var(imgui::StyleVar::FrameRounding(4.0));

        if let Some(_window) = ui
            .window("overlay")
            .position([width - 16.0, 16.0], condition)
            .position_pivot([1.0, 0.0])
            .title_bar(false)
            .scroll_bar(false)
            .no_nav()
            .movable(true)
            .resizable(true)
            .bg_alpha(0.8)
            .begin()
        {
            ui.set_window_font_scale(1.5);

            let _stretch = ui.push_item_width(-1.0);
            if let Some(_table) =
                ui.begin_table_with_flags("stats", 2, imgui::TableFlags::SIZING_FIXED_FIT)
            {
                stat_row(
                    ui,
                    "streaming res:",
                    format!("{}x{}", self.streaming_width, self.streaming_height),
                );

                stat_row(
                    ui,
                    "render res:",
                    format!("{}x{}", width * scale_x, height * scale_y),
                );

                stat_row(
                    ui,
                    "codec:",
                    match self.codec {
                        protocol::VideoCodec::H264 => "H.264",
                        protocol::VideoCodec::H265 => "H.265",
                        protocol::VideoCodec::Av1 => "AV1",
                        _ => "unknown",
                    },
                );

                stat_row(ui, "total latency:", format!("{:.1} ms", latency))
            }

            let [width, height] = ui.window_size();
            let cursor_pos = ui.cursor_pos();

            let measurements = self.video_latency_measurements.make_contiguous();
            let max_latency = measurements.iter().copied().reduce(f32::max).unwrap();
            let scale = (max_latency.round() as u32).next_multiple_of(10) * 2;

            ui.plot_lines("", measurements)
                .scale_min(0.0)
                .scale_max(scale as f32)
                .graph_size([width - 16.0, 50.0_f32.max(height - cursor_pos[1] - 8.0)])
                .overlay_text(format!("latency: {:.1} ms", latency).as_str())
                .build();
        }

        Ok(())
    }
}

fn stat_row(ui: &imgui::Ui, label: impl AsRef<str>, value: impl AsRef<str>) {
    ui.table_next_row();
    ui.table_next_column();
    let cursor_pos = ui.cursor_pos();
    let pos_x = cursor_pos[0] + ui.column_width(0) - ui.calc_text_size(&label)[0];
    if pos_x > cursor_pos[0] {
        ui.set_cursor_pos([pos_x, cursor_pos[1]]);
    }

    ui.text_colored([0.6, 0.6, 0.6, 1.0], label);
    ui.table_next_column();
    ui.text(value);
}
