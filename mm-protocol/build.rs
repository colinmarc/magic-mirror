// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

fn main() -> std::io::Result<()> {
    prost_build::Config::new()
        .bytes(["."])
        .include_file("_include.rs")
        .compile_protos(&["src/messages.proto"], &["src/"])?;
    Ok(())
}
