// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::{ffi::CString, path::PathBuf};

use xkbcommon::xkb;

extern crate slang;

fn main() {
    system_deps::Config::new().probe().unwrap();

    let mut session = slang::GlobalSession::new();
    let out_dir = std::env::var("OUT_DIR")
        .map(std::path::PathBuf::from)
        .expect("OUT_DIR not set");

    compile_shader(
        &mut session,
        "src/compositor/video/composite.slang",
        out_dir.join("shaders/composite_vert.spv").to_str().unwrap(),
        "vert",
        slang::Stage::Vertex,
        [],
    );

    compile_shader(
        &mut session,
        "src/compositor/video/composite.slang",
        out_dir.join("shaders/composite_frag.spv").to_str().unwrap(),
        "frag",
        slang::Stage::Fragment,
        [],
    );

    compile_shader(
        &mut session,
        "src/compositor/video/convert.slang",
        out_dir
            .join("shaders/convert_multiplanar.spv")
            .to_str()
            .unwrap(),
        "main",
        slang::Stage::Compute,
        [],
    );

    compile_shader(
        &mut session,
        "src/compositor/video/convert.slang",
        out_dir
            .join("shaders/convert_semiplanar.spv")
            .to_str()
            .unwrap(),
        "main",
        slang::Stage::Compute,
        [("SEMIPLANAR", "1")],
    );

    // We need a keymap for the compositor, but it shouldn't affect much, since we
    // operate generally with physical keycodes and so do games. If this proves
    // limiting, we could allow the configuration of other virtual keyboards.
    let xkb_ctx = xkb::Context::new(0);
    save_keymap(
        &xkb_ctx,
        out_dir.join("keymaps/iso_us.txt").to_str().unwrap(),
        "",
        "pc105",
        "us",
        "",
        None,
    );
}

fn compile_shader<'a>(
    session: &mut slang::GlobalSession,
    in_path: &str,
    out_path: &str,
    entry_point: &str,
    stage: slang::Stage,
    defines: impl IntoIterator<Item = (&'a str, &'a str)>,
) {
    std::fs::create_dir_all(PathBuf::from(out_path).parent().unwrap())
        .expect("failed to create output directory");

    let mut compile_request = session.create_compile_request();

    compile_request
        .add_search_path("../shader-common")
        .set_codegen_target(slang::CompileTarget::Spirv)
        .set_optimization_level(slang::OptimizationLevel::Maximal)
        .set_target_profile(session.find_profile("glsl_460"));

    for (name, value) in defines {
        compile_request.add_preprocessor_define(name, value);
    }

    let entry_point = compile_request
        .add_translation_unit(slang::SourceLanguage::Slang, None)
        .add_source_file(in_path)
        .add_entry_point(entry_point, stage);

    let shader_bytecode = compile_request
        .compile()
        .expect("Shader compilation failed.");

    std::fs::write(out_path, shader_bytecode.get_entry_point_code(entry_point))
        .expect("failed to write shader bytecode to file");

    println!("cargo::rerun-if-changed={}", in_path);
}

fn save_keymap(
    ctx: &xkb::Context,
    out_path: &str,
    rules: &str,
    model: &str,
    layout: &str,
    variant: &str,
    options: Option<&str>,
) {
    std::fs::create_dir_all(PathBuf::from(out_path).parent().unwrap())
        .expect("failed to create output directory");

    let keymap = xkb::Keymap::new_from_names(
        ctx,
        rules,
        model,
        layout,
        variant,
        options.map(|s| s.to_string()),
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .expect("failed to create keymap");

    let s = keymap.get_as_string(xkb::FORMAT_TEXT_V1);

    std::fs::write(out_path, CString::new(s).unwrap().to_bytes_with_nul())
        .expect("failed to write keymap bytes to file");
}
