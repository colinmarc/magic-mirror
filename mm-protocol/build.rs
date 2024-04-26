// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

fn main() -> std::io::Result<()> {
    let mut conf = prost_build::Config::new();

    #[cfg(feature = "uniffi")]
    conf.enum_attribute(".", "#[derive(uniffi::Enum)]");

    conf.bytes(["."])
        .include_file("_include.rs")
        .compile_protos(&["src/messages.proto"], &["src/"])?;

    Ok(())
}
