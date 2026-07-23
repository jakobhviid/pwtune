//! Profile storage: drafts/ (scratch) + calibrated/ (finalized), lookup, and the
//! PipeWire filter-chain config template.
use crate::dsp::Band;
use crate::ui::slugify;
use anyhow::{anyhow, Result};
use regex::Regex;
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Draft,
    Shipped,
}

/// Repo/data root: $PWTUNE_HOME if set (the justfile exports it), else CWD.
pub fn home() -> PathBuf {
    std::env::var("PWTUNE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

pub fn drafts_dir() -> PathBuf { home().join("drafts") }
pub fn calibrated_dir() -> PathBuf { home().join("calibrated") }

pub fn conf_d() -> PathBuf {
    let h = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(h).join(".config/pipewire/pipewire.conf.d")
}

pub const DEPLOY_PREFIX: &str = "pwtune-";

fn names_in(dir: &PathBuf) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            n.strip_suffix(".conf").map(|s| s.to_string())
        })
        .collect();
    v.sort();
    v
}

pub fn draft_names() -> Vec<String> { names_in(&drafts_dir()) }
pub fn calibrated_names() -> Vec<String> { names_in(&calibrated_dir()) }

/// All profile names, drafts first (active work), de-duplicated.
pub fn profile_names() -> Vec<String> {
    let mut out = Vec::new();
    for n in draft_names().into_iter().chain(calibrated_names()) {
        if !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

/// (path, tier) for a name, preferring a draft over a shipped one.
pub fn find_profile(name: &str) -> Option<(PathBuf, Tier)> {
    let d = drafts_dir().join(format!("{name}.conf"));
    if d.is_file() {
        return Some((d, Tier::Draft));
    }
    let c = calibrated_dir().join(format!("{name}.conf"));
    if c.is_file() {
        return Some((c, Tier::Shipped));
    }
    None
}

fn field(path: &PathBuf, name: &str) -> Option<String> {
    let txt = fs::read_to_string(path).ok()?;
    let re = Regex::new(&format!(r"(?m)^#\s*{}:\s*(.+?)\s*$", regex::escape(name))).ok()?;
    re.captures(&txt).map(|c| c[1].to_string())
}

pub fn target_match(path: &PathBuf) -> Option<String> { field(path, "target-match") }

pub fn description(path: &PathBuf) -> String {
    fs::read_to_string(path)
        .ok()
        .and_then(|t| {
            Regex::new(r#"node\.description\s*=\s*"([^"]+)""#)
                .ok()
                .and_then(|re| re.captures(&t).map(|c| c[1].to_string()))
        })
        .unwrap_or_else(|| "(no description)".into())
}

pub fn input_node(path: &PathBuf) -> Option<String> {
    let t = fs::read_to_string(path).ok()?;
    Regex::new(r"effect_input\.[A-Za-z0-9_]+")
        .ok()?
        .find(&t)
        .map(|m| m.as_str().to_string())
}

/// Write a profile config (drafts/<name>.conf). Mirrors the Python template.
pub fn write_profile(name: &str, bands: &[Band], boost: f64, target: &str, out: &PathBuf) -> Result<()> {
    let slug = slugify(name);
    let mut nodes: Vec<String> = Vec::new();
    let mut links: Vec<String> = Vec::new();
    let mut prev: Option<String> = None;

    let add = |node_name: &str, label: &str, freq: f64, q: f64, gain: f64, comment: &str,
                   nodes: &mut Vec<String>, links: &mut Vec<String>, prev: &mut Option<String>| {
        nodes.push(format!(
            "                    {{\n\
             \x20                       type  = builtin\n\
             \x20                       name  = {node_name}\n\
             \x20                       label = {label}\n\
             \x20                       control = {{\n\
             \x20                           # {comment}\n\
             \x20                           \"Freq\"  = {freq:.1}\n\
             \x20                           \"Q\"     = {q}\n\
             \x20                           \"Gain\"  = {gain}\n\
             \x20                       }}\n\
             \x20                   }}"
        ));
        if let Some(p) = prev {
            links.push(format!("                    {{ output = \"{p}:Out\" input = \"{node_name}:In\" }}"));
        }
        *prev = Some(node_name.to_string());
    };

    if boost > 0.0 {
        let half = (boost / 2.0 * 10.0).round() / 10.0;
        add("preamp1", "bq_lowshelf", 20000.0, 0.7, half,
            &format!("Makeup gain: +{half} dB (part 1 of +{boost} dB total)"),
            &mut nodes, &mut links, &mut prev);
        add("preamp2", "bq_highshelf", 20.0, 0.7, half,
            &format!("Makeup gain: +{half} dB (part 2 of +{boost} dB total)"),
            &mut nodes, &mut links, &mut prev);
    }
    for (i, b) in bands.iter().enumerate() {
        add(&format!("eq_band{}", i + 1), b.kind, b.freq, b.q, b.gain,
            &format!("auto: correct toward target at {} Hz", b.freq),
            &mut nodes, &mut links, &mut prev);
    }

    let header = format!(
        "# {name} — PipeWire filter-chain speaker EQ\n\
         # Auto-generated by pwtune; edit freely and re-measure to refine.\n\
         # Makeup gain (preamp): +{boost} dB   |   {} EQ band(s)\n\
         #\n\
         # target-match: {target}\n\
         #   `pwtune install` resolves this to the actual sink node.name (it\n\
         #   differs per machine/GPU/bus, but the monitor/device name is stable). If\n\
         #   you deploy this by hand, replace __TARGET_SINK__ with a name from\n\
         #   `pactl list short sinks`.\n\n",
        bands.len()
    );

    let graph = format!(
        "{header}context.modules = [\n\
         \x20   {{ name = libpipewire-module-filter-chain\n\
         \x20       args = {{\n\
         \x20           node.description = \"{name}\"\n\
         \x20           media.name        = \"{name}\"\n\
         \x20           filter.graph = {{\n\
         \x20               nodes = [\n{}\n\
         \x20               ]\n\
         \x20               links = [\n{}\n\
         \x20               ]\n\
         \x20           }}\n\
         \x20           capture.props = {{\n\
         \x20               node.name   = \"effect_input.eq_{slug}\"\n\
         \x20               media.class = Audio/Sink\n\
         \x20               audio.channels = 2\n\
         \x20               audio.position = [ FL FR ]\n\
         \x20           }}\n\
         \x20           playback.props = {{\n\
         \x20               node.name   = \"effect_output.eq_{slug}\"\n\
         \x20               node.target = \"__TARGET_SINK__\"\n\
         \x20               audio.channels = 2\n\
         \x20               audio.position = [ FL FR ]\n\
         \x20           }}\n\
         \x20       }}\n\
         \x20   }}\n\
         ]\n",
        nodes.join("\n"),
        links.join("\n"),
    );

    if let Some(parent) = out.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(out, graph).map_err(|e| anyhow!("write {}: {e}", out.display()))?;
    Ok(())
}
