// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

#![allow(non_upper_case_globals)]
#![allow(non_camel_case_types)]

use wayland_server;
use wayland_server::protocol::*;

pub mod __interfaces {
    use wayland_server::backend as wayland_backend;
    use wayland_server::protocol::__interfaces::*;
    wayland_scanner::generate_interfaces!("src/compositor/protocols/game-controller-v1.xml");
}

use self::__interfaces::*;
wayland_scanner::generate_server_code!("src/compositor/protocols/game-controller-v1.xml");

pub use wp_game_controller_v1::*;
