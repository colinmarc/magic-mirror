//! Generates markdown docs from mmserver.default.toml. Tightly coupled
//! to the format of that file.

use std::{
    fs::File,
    io::{BufRead as _, BufReader},
};

use regex::Regex;

const FRONT_MATTER: &str = r#"
+++
title = "Configuration Reference"

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

    let mut preamble = true;
    let mut key_path: Vec<String> = Vec::new();
    let mut docs = Vec::new();

    let keypath_section_re = Regex::new(r"\A#?\s*\[([a-z0-9-_.]+)\]\s*\z").unwrap();
    let key_re = Regex::new(r"\A(#?)\s*([a-z0-9-_]+)\s=\s(.*)\z").unwrap();

    println!("{}", FRONT_MATTER);

    for line in r.lines() {
        let s = line.expect("io error");
        if s.is_empty() {
            preamble = false;

            for doc in docs.drain(..) {
                println!("{}", doc);
            }

            continue;
        } else if preamble {
            continue;
        }

        if let Some(header) = s.strip_prefix("## *** ") {
            // Documentation sections.
            println!("\n## {}", header.strip_suffix(" ***").unwrap());
        } else if s.starts_with("## ***") {
            // Section decoration.
            continue;
        } else if let Some(doc) = s.strip_prefix("##") {
            // Key documentation.
            docs.push(doc.trim_start().to_owned());
        } else if let Some(m) = key_re.captures(&s) {
            // Key, value.
            let is_default = m.get(1).unwrap().is_empty();
            let key = m.get(2).unwrap().as_str();
            let value = m.get(3).unwrap().as_str();

            let full_path = key_path
                .iter()
                .map(String::as_str)
                .chain(key.split('.'))
                .collect::<Vec<_>>()
                .join(".");

            println!("\n#### `{}`\n", full_path);
            if is_default {
                println!("```toml\n# Default\n{} = {}\n```\n", key, value);
            } else {
                println!(
                    "```toml\n# Example (default unset)\n{} = {}\n```\n",
                    key, value
                );
            }

            for doc in docs.drain(..) {
                println!("{}", doc);
            }
        } else if let Some(m) = keypath_section_re.captures(&s) {
            // Update keypath for TOML section headers.
            key_path.clear();
            for key in m.get(1).unwrap().as_str().split(".") {
                // Example app becomes <app name> in the docs.
                if key == "steam-big-picture" {
                    key_path.push("<app name>".to_owned());
                } else {
                    key_path.push(key.to_owned());
                }
            }
        } else {
            eprintln!("error: unmatched line: \n{}", s);
            std::process::exit(1);
        }
    }
}
