//! Everything that talks to PipeWire via pactl/paplay/parecord and systemctl.
use crate::dsp::RATE;
use crate::profile::{conf_d, DEPLOY_PREFIX};
use anyhow::Result;
use regex::Regex;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

pub struct Device {
    pub name: String,
    pub desc: String,
    pub nick: String,
}

fn pactl(args: &[&str]) -> String {
    Command::new("pactl")
        .args(args)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

/// Parse `pactl list sinks|sources` into devices (name, description, node.nick).
pub fn get_devices(kind: &str) -> Vec<Device> {
    let out = pactl(&["list", kind]);
    let mut devs = Vec::new();
    let mut cur: Option<Device> = None;
    for line in out.lines() {
        let s = line.trim();
        if let Some(rest) = s.strip_prefix("Name:") {
            if let Some(d) = cur.take() {
                devs.push(d);
            }
            cur = Some(Device { name: rest.trim().to_string(), desc: String::new(), nick: String::new() });
        } else if let Some(rest) = s.strip_prefix("Description:") {
            if let Some(d) = cur.as_mut() {
                d.desc = rest.trim().to_string();
            }
        } else if let Some(rest) = s.strip_prefix("node.nick") {
            if let Some(d) = cur.as_mut() {
                d.nick = rest.trim_start_matches([' ', '=']).trim().trim_matches('"').to_string();
            }
        }
    }
    if let Some(d) = cur {
        devs.push(d);
    }
    devs
}

fn ci_contains(hay: &str, needle: &str) -> bool {
    hay.to_lowercase().contains(&needle.to_lowercase())
}

pub fn detect_speaker(devs: &[Device]) -> Option<usize> {
    for (i, d) in devs.iter().enumerate() {
        let blob = format!("{} {} {}", d.name, d.desc, d.nick);
        if ci_contains(&blob, "speaker") && !ci_contains(&d.name, "monitor") {
            return Some(i);
        }
    }
    for (i, d) in devs.iter().enumerate() {
        if (ci_contains(&d.name, "sof") || ci_contains(&d.name, "hda") || ci_contains(&d.name, "analog") || ci_contains(&d.name, "hdmi"))
            && !ci_contains(&d.name, "monitor")
        {
            return Some(i);
        }
    }
    None
}

pub fn detect_mic(devs: &[Device]) -> Option<usize> {
    for (i, d) in devs.iter().enumerate() {
        let blob = format!("{} {} {}", d.name, d.desc, d.nick);
        if ci_contains(&blob, "mic") && !ci_contains(&blob, "headset") {
            return Some(i);
        }
    }
    if !devs.is_empty() {
        Some(0)
    } else {
        None
    }
}

/// Sinks (excluding our own effect_* virtual sinks) whose full `pactl list sinks`
/// block matches `pattern` case-insensitively — so node.nick / EDID name counts too.
pub fn resolve_sink(pattern: &str) -> Vec<String> {
    let out = pactl(&["list", "sinks"]);
    let re = match Regex::new(&format!("(?i){pattern}")) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let name_re = Regex::new(r"Name:\s*(\S+)").unwrap();
    let mut hits = Vec::new();
    for chunk in out.split("\nSink #") {
        if chunk.trim().is_empty() || !re.is_match(chunk) {
            continue;
        }
        if let Some(c) = name_re.captures(chunk) {
            let n = c[1].to_string();
            if !n.starts_with("effect_input.") && !n.starts_with("effect_output.") && !hits.contains(&n) {
                hits.push(n);
            }
        }
    }
    hits
}

pub fn get_default_sink() -> String { pactl(&["get-default-sink"]).trim().to_string() }
pub fn set_default_sink(name: &str) -> bool {
    Command::new("pactl").args(["set-default-sink", name]).status().map(|s| s.success()).unwrap_or(false)
}
pub fn set_default_source(name: &str) {
    let _ = Command::new("pactl").args(["set-default-source", name]).status();
}

pub fn restart_pipewire() {
    let _ = Command::new("systemctl")
        .args(["--user", "restart", "pipewire", "pipewire-pulse"])
        .status();
    sleep(Duration::from_millis(1500));
}

/// Play the sweep out `speaker` while recording from `mic` into `rec_path`.
pub fn play_and_record(sweep: &str, rec_path: &str, speaker: &str, mic: &str) -> Result<()> {
    set_default_source(mic);
    let mut rec = Command::new("parecord")
        .args([
            "--format=s16le",
            &format!("--rate={RATE}"),
            "--channels=1",
            &format!("--device={mic}"),
            "--file-format=wav",
            rec_path,
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    sleep(Duration::from_millis(1000));
    let _ = Command::new("paplay")
        .args([&format!("--device={speaker}"), sweep])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    sleep(Duration::from_millis(1500));
    // SIGTERM (not kill) so parecord finalizes the WAV header.
    let _ = Command::new("kill").args(["-TERM", &rec.id().to_string()]).status();
    let _ = rec.wait();
    Ok(())
}

pub enum Kind {
    /// Deployed by pwtune (a pwtune-<name>.conf file).
    Managed,
    /// Any other EQ filter-chain config on the system (e.g. a leftover, or one
    /// deployed by another tool). uninstall is system-wide, so it surfaces these
    /// too — but it can't tie a deployed file back to a source folder, so there
    /// are no draft/calibrated tiers here.
    External,
}

/// Every EQ filter-chain config currently deployed, so uninstall can clean up
/// anything lingering. Returns (label, path, kind).
pub fn deployed_eq_configs() -> Vec<(String, PathBuf, Kind)> {
    let dir = conf_d();
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        Err(_) => return out,
    };
    let mut files: Vec<String> = entries
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            if n.ends_with(".conf") { Some(n) } else { None }
        })
        .collect();
    files.sort();
    for f in files {
        let path = dir.join(&f);
        if let Some(name) = f.strip_prefix(DEPLOY_PREFIX).and_then(|s| s.strip_suffix(".conf")) {
            out.push((name.to_string(), path, Kind::Managed));
        } else if let Ok(txt) = std::fs::read_to_string(&path) {
            if txt.contains("libpipewire-module-filter-chain") && txt.contains("effect_input") {
                out.push((f.clone(), path, Kind::External));
            }
        }
    }
    out
}
