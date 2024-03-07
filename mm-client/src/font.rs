// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use font_kit::{
    family_name::FamilyName,
    font::Font,
    properties::{Properties, Weight},
    source::SystemSource,
};
use tracing::debug;

pub fn load_ui_font() -> anyhow::Result<Font> {
    let font = SystemSource::new()
        .select_best_match(
            &[FamilyName::Monospace, FamilyName::SansSerif],
            Properties::new().weight(Weight::THIN),
        )?
        .load()?;

    debug!("font: {:?}", font);

    Ok(font)
}

// #[cfg(target_os = "macos")]
// pub fn load_ui_font() -> anyhow::Result<Font> {
//     let ctf = core_text::font::new_ui_font_for_language();

//     let font = unsafe { Font::from_native_font(ctf) };
//     Ok(font)
// }
