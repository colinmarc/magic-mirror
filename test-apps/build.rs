// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: MIT

extern crate shaderc;

fn main() {
    let mut compiler = shaderc::Compiler::new().unwrap();
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let out_dir = std::path::Path::new(&out_dir);

    std::fs::create_dir_all(out_dir.join("shaders")).unwrap();

    compile_shader(
        &mut compiler,
        "src/shaders/vert.glsl",
        out_dir.join("shaders/vert.spv").to_str().unwrap(),
        shaderc::ShaderKind::Vertex,
    );

    compile_shader(
        &mut compiler,
        "src/shaders/frag.glsl",
        out_dir.join("shaders/frag.spv").to_str().unwrap(),
        shaderc::ShaderKind::Fragment,
    );
}

fn compile_shader(
    compiler: &mut shaderc::Compiler,
    in_path: &str,
    out_path: &str,
    kind: shaderc::ShaderKind,
) {
    let source = match std::fs::read_to_string(in_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to read {}: {}", in_path, e);
            std::process::exit(1);
        }
    };

    let artifact = match compiler.compile_into_spirv(&source, kind, in_path, "main", None) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{}", e);
            std::process::exit(1);
        }
    };

    println!("cargo:rerun-if-changed={in_path}");
    std::fs::write(out_path, artifact.as_binary_u8()).unwrap();
}
