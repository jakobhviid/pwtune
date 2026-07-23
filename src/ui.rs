//! Terminal UI: logging, prompts, sectioned pickers, "next step" hints.
use std::io::{self, BufRead, Write};

pub fn info(m: &str) { println!("\x1b[1;34m▸ {m}\x1b[0m"); }
pub fn ok(m: &str) { println!("\x1b[1;32m✓ {m}\x1b[0m"); }
pub fn warn(m: &str) { println!("\x1b[1;33m⚠ {m}\x1b[0m"); }
pub fn err(m: &str) { eprintln!("\x1b[1;31m✗ {m}\x1b[0m"); }

fn read_line() -> String {
    let mut s = String::new();
    match io::stdin().lock().read_line(&mut s) {
        Ok(0) | Err(_) => String::new(), // EOF or error -> empty
        Ok(_) => s.trim().to_string(),
    }
}

/// Free-text prompt with an optional default (Enter accepts it).
pub fn ask(prompt: &str, default: &str) -> String {
    if default.is_empty() {
        eprint!("{prompt}: ");
    } else {
        eprint!("{prompt} [{default}]: ");
    }
    let _ = io::stderr().flush();
    let ans = read_line();
    if ans.is_empty() { default.to_string() } else { ans }
}

/// Flat numbered picker. Enter accepts default; a typed name is echoed as-is.
pub fn pick(prompt: &str, choices: &[String], default: Option<&str>) -> Option<String> {
    eprintln!("Available:");
    for (i, c) in choices.iter().enumerate() {
        eprintln!("  {}) {c}", i + 1);
    }
    prompt_index(prompt, choices, default)
}

/// Picker split into labelled sections (e.g. drafts on top, calibrated below).
/// Numbering is continuous so a single number selects.
pub fn pick_sections(
    prompt: &str,
    sections: &[(&str, Vec<String>)],
    default: Option<&str>,
) -> Option<String> {
    let mut flat: Vec<String> = Vec::new();
    eprintln!();
    for (header, names) in sections {
        if names.is_empty() {
            continue;
        }
        eprintln!("  {header}:");
        for n in names {
            let mark = if default == Some(n.as_str()) { "   ← default (Enter)" } else { "" };
            eprintln!("    {}) {n}{mark}", flat.len() + 1);
            flat.push(n.clone());
        }
    }
    if flat.is_empty() {
        return None;
    }
    prompt_index(prompt, &flat, default)
}

fn prompt_index(prompt: &str, choices: &[String], default: Option<&str>) -> Option<String> {
    match default {
        Some(d) => eprint!("{prompt} [{d}]: "),
        None => eprint!("{prompt}: "),
    }
    let _ = io::stderr().flush();
    let ans = read_line();
    if ans.is_empty() {
        return default.map(|s| s.to_string());
    }
    if let Ok(n) = ans.parse::<usize>() {
        if n >= 1 && n <= choices.len() {
            return Some(choices[n - 1].clone());
        }
    }
    Some(ans)
}

/// How the user invoked us, so "next step" hints are copy-pasteable.
/// The justfile exports SPEAKER_EQ_LAUNCHER=just; otherwise it's the binary name.
pub fn run_cmd(verb: &str, arg: &str) -> String {
    let launcher = std::env::var("SPEAKER_EQ_LAUNCHER").unwrap_or_else(|_| "speaker-eq".into());
    if arg.is_empty() {
        format!("{launcher} {verb}")
    } else {
        format!("{launcher} {verb} {arg}")
    }
}

/// Print a short "what to do next" block. Items are (description, command).
pub fn next_steps(items: &[(&str, String)]) {
    println!("\n\x1b[1;34m  Next:\x1b[0m");
    for (desc, cmd) in items {
        if cmd.is_empty() {
            println!("    • {desc}");
        } else {
            println!("    • {desc}:  {cmd}");
        }
    }
}

/// lowercase, non-alnum runs -> single underscore, trimmed. For node names.
pub fn slugify(name: &str) -> String {
    let mut out = String::new();
    let mut prev_us = false;
    for ch in name.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_us = false;
        } else if !prev_us {
            out.push('_');
            prev_us = true;
        }
    }
    out.trim_matches('_').to_string()
}
