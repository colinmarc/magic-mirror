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
    let mut comment_lines = Vec::new();
    for line in r
        .lines()
        .skip_while(|s| !s.as_ref().unwrap().starts_with("// # "))
    {
        let line = line.unwrap();
        if message_lines.is_empty() && line.is_empty() {
            emit_comments(&mut comment_lines);
            println!();
        } else if let Some(comment) = line.strip_prefix("// ").or_else(|| line.strip_prefix("//")) {
            emit_message_code_block(&mut message_lines);
            comment_lines.push(comment.to_owned());
        } else if !line.contains("TODO") {
            emit_comments(&mut comment_lines);
            message_lines.push(line);
        }
    }

    emit_comments(&mut comment_lines);
    emit_message_code_block(&mut message_lines);
}

fn emit_comments(lines: &mut Vec<String>) {
    let comment = lines.join("\n");

    // Add internal links.
    let comment = regex::Regex::new(r"`(?s)(\d+)\s+-\s+([\w\s]+)`")
        .unwrap()
        .replace_all(&comment, |caps: &regex::Captures<'_>| {
            let slug = caps[2]
                .to_lowercase()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join("-");

            format!("[{}](#{}-{})", &caps[0], &caps[1], slug)
        });

    println!("{}", comment);
    lines.clear();
}

fn emit_message_code_block(lines: &mut Vec<String>) {
    if !lines.is_empty() {
        let message = lines.join("\n");
        println!("\n```proto\n{}\n```\n", message.trim());
        lines.clear();
    }
}
