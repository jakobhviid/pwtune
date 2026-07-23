//! Profiles are plain `.conf` files in the CURRENT directory — pwtune is a
//! folder-scoped tool (like a linter): it works on whatever folder you run it in.
//! A profile is "calibrated" (frozen) purely by its filename: `foo.calibrated.conf`.
//! `promote` just renames `foo.conf` -> `foo.calibrated.conf`; edit/delete refuse
//! a calibrated file. The display name strips the marker (`foo`), shown with a tag.
use crate::dsp::Band;
use crate::ui::slugify;
use anyhow::{anyhow, Result};
use regex::Regex;
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Draft,
    Calibrated,
}

pub const DEPLOY_PREFIX: &str = "pwtune-";

pub fn conf_d() -> PathBuf {
    let h = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(h).join(".config/pipewire/pipewire.conf.d")
}

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// filename stem -> (display name, tier). `foo.calibrated` -> ("foo", Calibrated).
fn classify(stem: &str) -> (String, Tier) {
    match stem.strip_suffix(".calibrated") {
        Some(base) => (base.to_string(), Tier::Calibrated),
        None => (stem.to_string(), Tier::Draft),
    }
}

/// Every profile in the current directory: (name, path, tier), sorted by name.
pub fn scan() -> Vec<(String, PathBuf, Tier)> {
    let mut out: Vec<(String, PathBuf, Tier)> = fs::read_dir(cwd())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let f = e.file_name().to_string_lossy().to_string();
            f.strip_suffix(".conf").map(|stem| {
                let (name, tier) = classify(stem);
                (name, e.path(), tier)
            })
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn names_of(tier: Tier) -> Vec<String> {
    let mut v: Vec<String> =
        scan().into_iter().filter(|(_, _, t)| *t == tier).map(|(n, _, _)| n).collect();
    v.dedup();
    v
}

pub fn draft_names() -> Vec<String> {
    names_of(Tier::Draft)
}

pub fn calibrated_names() -> Vec<String> {
    names_of(Tier::Calibrated)
}

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

/// (path, tier) for a name, preferring a draft over a calibrated one.
pub fn find_profile(name: &str) -> Option<(PathBuf, Tier)> {
    let d = draft_path(name);
    if d.is_file() {
        return Some((d, Tier::Draft));
    }
    let c = calibrated_path(name);
    if c.is_file() {
        return Some((c, Tier::Calibrated));
    }
    None
}

/// Where `create` writes, and the source `promote` renames.
pub fn draft_path(name: &str) -> PathBuf {
    cwd().join(format!("{name}.conf"))
}

pub fn calibrated_path(name: &str) -> PathBuf {
    cwd().join(format!("{name}.calibrated.conf"))
}

fn field(path: &PathBuf, name: &str) -> Option<String> {
    let txt = fs::read_to_string(path).ok()?;
    let re = Regex::new(&format!(r"(?m)^#\s*{}:\s*(.+?)\s*$", regex::escape(name))).ok()?;
    re.captures(&txt).map(|c| c[1].to_string())
}

pub fn target_match(path: &PathBuf) -> Option<String> {
    field(path, "target-match")
}

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

/// Write a profile config. Mirrors the template used by the original tool.
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
            links.push(format!(
                "                    {{ output = \"{p}:Out\" input = \"{node_name}:In\" }}"
            ));
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
         #   `pwtune install` resolves this to the actual sink node.name (it differs\n\
         #   per machine/GPU/bus, but the monitor/device name is stable). If you\n\
         #   deploy this by hand, replace __TARGET_SINK__ with a name from\n\
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

    fs::write(out, graph).map_err(|e| anyhow!("write {}: {e}", out.display()))?;
    Ok(())
}
