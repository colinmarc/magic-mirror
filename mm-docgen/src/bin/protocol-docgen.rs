//! Generates markdown docs from mm-protoco/src/messages.proto. Tightly coupled
//! to the format of that file.

use std::{
    fs::File,
    io::{BufRead as _, BufReader},
};

const FRONT_MATTER: &str = r#"
+++
title = "Protocol Reference"

[extra]
toc = true
+++
"#;

fn main() {
    let mut args = std::env::args();

    if args.len() != 2 {
        eprintln!("usage: {} SRC", args.next().unwrap());
        std::process::exit(1);
    }

    let _ = args.next().unwrap();
    let src = args.next().unwrap();

    let r = BufReader::new(File::open(src).expect("source path does not exist"));

    println!("{}", FRONT_MATTER);

    // Skip until the first <h1>.
    let mut message_lines = Vec::new();
    for line in r
        .lines()
        .skip_while(|s| !s.as_ref().unwrap().starts_with("// # "))
    {
        let line = line.unwrap();
        if message_lines.is_empty() && line.is_empty() {
            println!();
        } else if let Some(comment) = line.strip_prefix("// ").or_else(|| line.strip_prefix("//")) {
            // Emit a code block.
            emit_message_code_block(&mut message_lines);

            println!("{}", comment);
        } else if !line.contains("TODO") {
            message_lines.push(line);
        }
    }

    emit_message_code_block(&mut message_lines);
}

fn emit_message_code_block(lines: &mut Vec<String>) {
    if !lines.is_empty() {
        let message = lines.join("\n");
        println!("\n```proto\n{}\n```\n", message.trim());
        lines.clear();
    }
}
