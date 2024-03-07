// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

extern crate shaderc;

fn main() {
    system_deps::Config::new().probe().unwrap();

    let mut compiler = shaderc::Compiler::new().unwrap();
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let out_dir = std::path::Path::new(&out_dir);

    std::fs::create_dir_all(out_dir.join("shaders")).unwrap();

    compile_shader(
        &mut compiler,
        "src/compositor/video/shaders/composite_vert.glsl",
        out_dir.join("shaders/composite_vert.spv").to_str().unwrap(),
        shaderc::ShaderKind::Vertex,
        [],
    );

    compile_shader(
        &mut compiler,
        "src/compositor/video/shaders/composite_frag.glsl",
        out_dir.join("shaders/composite_frag.spv").to_str().unwrap(),
        shaderc::ShaderKind::Fragment,
        [],
    );

    compile_shader(
        &mut compiler,
        "src/compositor/video/shaders/convert.glsl",
        out_dir
            .join("shaders/convert_multiplanar.spv")
            .to_str()
            .unwrap(),
        shaderc::ShaderKind::Compute,
        [],
    );

    compile_shader(
        &mut compiler,
        "src/compositor/video/shaders/convert.glsl",
        out_dir
            .join("shaders/convert_semiplanar.spv")
            .to_str()
            .unwrap(),
        shaderc::ShaderKind::Compute,
        [("SEMIPLANAR", "1")],
    );
}

fn compile_shader<'a>(
    compiler: &mut shaderc::Compiler,
    in_path: &str,
    out_path: &str,
    kind: shaderc::ShaderKind,
    opts: impl IntoIterator<Item = (&'a str, &'a str)>,
) {
    let source = match std::fs::read_to_string(in_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to read {}: {}", in_path, e);
            std::process::exit(1);
        }
    };

    let mut compile_opts = shaderc::CompileOptions::new().unwrap();
    for (k, v) in opts {
        compile_opts.add_macro_definition(k.as_ref(), Some(v));
    }

    let artifact =
        match compiler.compile_into_spirv(&source, kind, in_path, "main", Some(&compile_opts)) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        };

    println!("cargo:rerun-if-changed={in_path}");
    std::fs::write(out_path, artifact.as_binary_u8()).unwrap();
}
