// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

use std::path::PathBuf;

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
