// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

use mm_protocol as protocol;
use winit::window::{CursorIcon, CustomCursor, CustomCursorSource};

pub fn load_cursor_image(image: &[u8], hs_x: u32, hs_y: u32) -> anyhow::Result<CustomCursorSource> {
    let cursor = image::load_from_memory_with_format(image, image::ImageFormat::Png)?;

    let w = cursor.width().try_into()?;
    let h = cursor.height().try_into()?;
    let hs_x = hs_x.try_into()?;
    let hs_y = hs_y.try_into()?;

    Ok(CustomCursor::from_rgba(
        cursor.to_rgba8().into_raw(),
        w,
        h,
        hs_x,
        hs_y,
    )?)
}

pub fn cursor_icon_from_proto(icon: protocol::update_cursor::CursorIcon) -> CursorIcon {
    match icon {
        protocol::update_cursor::CursorIcon::ContextMenu => CursorIcon::ContextMenu,
        protocol::update_cursor::CursorIcon::Help => CursorIcon::Help,
        protocol::update_cursor::CursorIcon::Pointer => CursorIcon::Pointer,
        protocol::update_cursor::CursorIcon::Progress => CursorIcon::Progress,
        protocol::update_cursor::CursorIcon::Wait => CursorIcon::Wait,
        protocol::update_cursor::CursorIcon::Cell => CursorIcon::Cell,
        protocol::update_cursor::CursorIcon::Crosshair => CursorIcon::Crosshair,
        protocol::update_cursor::CursorIcon::Text => CursorIcon::Text,
        protocol::update_cursor::CursorIcon::VerticalText => CursorIcon::VerticalText,
        protocol::update_cursor::CursorIcon::Alias => CursorIcon::Alias,
        protocol::update_cursor::CursorIcon::Copy => CursorIcon::Copy,
        protocol::update_cursor::CursorIcon::Move => CursorIcon::Move,
        protocol::update_cursor::CursorIcon::NoDrop => CursorIcon::NoDrop,
        protocol::update_cursor::CursorIcon::NotAllowed => CursorIcon::NotAllowed,
        protocol::update_cursor::CursorIcon::Grab => CursorIcon::Grab,
        protocol::update_cursor::CursorIcon::Grabbing => CursorIcon::Grabbing,
        protocol::update_cursor::CursorIcon::EResize => CursorIcon::EResize,
        protocol::update_cursor::CursorIcon::NResize => CursorIcon::NResize,
        protocol::update_cursor::CursorIcon::NeResize => CursorIcon::NeResize,
        protocol::update_cursor::CursorIcon::NwResize => CursorIcon::NwResize,
        protocol::update_cursor::CursorIcon::SResize => CursorIcon::SResize,
        protocol::update_cursor::CursorIcon::SeResize => CursorIcon::SeResize,
        protocol::update_cursor::CursorIcon::SwResize => CursorIcon::SwResize,
        protocol::update_cursor::CursorIcon::WResize => CursorIcon::WResize,
        protocol::update_cursor::CursorIcon::EwResize => CursorIcon::EwResize,
        protocol::update_cursor::CursorIcon::NsResize => CursorIcon::NsResize,
        protocol::update_cursor::CursorIcon::NeswResize => CursorIcon::NeswResize,
        protocol::update_cursor::CursorIcon::NwseResize => CursorIcon::NwseResize,
        protocol::update_cursor::CursorIcon::ColResize => CursorIcon::ColResize,
        protocol::update_cursor::CursorIcon::RowResize => CursorIcon::RowResize,
        protocol::update_cursor::CursorIcon::AllScroll => CursorIcon::AllScroll,
        protocol::update_cursor::CursorIcon::ZoomIn => CursorIcon::ZoomIn,
        protocol::update_cursor::CursorIcon::ZoomOut => CursorIcon::ZoomOut,
        _ => CursorIcon::Default,
    }
}
