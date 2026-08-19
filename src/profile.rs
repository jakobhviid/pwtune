//! Profiles are `.conf` files in the CURRENT directory — pwtune is a folder-scoped
//! tool (like a linter): it works on whatever folder you run it in. To avoid
//! touching unrelated PipeWire configs, pwtune only recognizes its own two
//! suffixes:
//!   * `foo.draft.conf`      — a draft: editable, deletable
//!   * `foo.calibrated.conf` — frozen: edit/delete refuse it
//! `promote` renames `foo.draft.conf` -> `foo.calibrated.conf`. The display name
//! strips the marker (`foo`) and shows a tier tag. Any other `.conf` is ignored.
use crate::dsp::{Alignment, Band};
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

/// Where PipeWire reads a user's config fragments from.
///
/// It resolves `$XDG_CONFIG_HOME/pipewire` first and falls back to
/// `$HOME/.config/pipewire`, so a tool that only ever looks at the second one writes
/// files the daemon never reads — and then blames the result on the user's sound
/// settings. A relative `XDG_CONFIG_HOME` is ignored, as the spec requires.
pub fn conf_d() -> PathBuf {
    let leaf = |base: PathBuf| base.join("pipewire").join("pipewire.conf.d");
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .map(leaf);
    let home = leaf(PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config"));

    // PipeWire falls back to ~/.config when the XDG path does not exist, so an
    // existing directory wins: writing into a freshly created XDG directory would not
    // just miss the daemon's current location, it would move it — and silently orphan
    // whatever is already deployed under ~/.config.
    match xdg {
        Some(x) if x.is_dir() => x,
        Some(x) => {
            if home.is_dir() {
                home
            } else {
                x
            }
        }
        None => home,
    }
}

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Every pwtune profile in the current directory: (name, path, tier), sorted.
/// Only `*.draft.conf` and `*.calibrated.conf` are ours — any other `.conf` is
/// ignored, so pwtune never touches a config it didn't create.
pub fn scan() -> Vec<(String, PathBuf, Tier)> {
    let mut out: Vec<(String, PathBuf, Tier)> = fs::read_dir(cwd())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let f = e.file_name().to_string_lossy().to_string();
            if let Some(name) = f.strip_suffix(".draft.conf") {
                Some((name.to_string(), e.path(), Tier::Draft))
            } else if let Some(name) = f.strip_suffix(".calibrated.conf") {
                Some((name.to_string(), e.path(), Tier::Calibrated))
            } else {
                None
            }
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

/// Reject a name that would escape the folder, or collide with another profile.
///
/// Both node names come from `slugify`, which collapses every run of
/// non-alphanumerics to `_`, so "my desk", "my-desk" and "My.Desk" all become
/// `my_desk`. PipeWire never checks `node.name` for uniqueness: two chains carrying
/// the same one both load, both appear as sinks under that name, and which one plays
/// depends on registration order — non-deterministic across restarts. The name is
/// also interpolated straight into both the draft filename and the deployed one.
pub fn validate_name(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        return Err(anyhow!("a profile needs a name"));
    }
    if name.contains('/') || name.contains('\\') || name.split('.').any(|p| p == "..") {
        return Err(anyhow!(
            "'{name}' is not a usable profile name: it becomes a filename in this folder \
             and in PipeWire's config directory"
        ));
    }
    if name.starts_with('.') {
        return Err(anyhow!("'{name}' starts with a dot, which hides the profile"));
    }
    let slug = slugify(name);
    if slug.is_empty() {
        return Err(anyhow!("'{name}' has no letters or digits, so it cannot name a sink"));
    }
    for (other, _, _) in scan() {
        if other != name && slugify(&other) == slug {
            return Err(anyhow!(
                "'{name}' and the existing profile '{other}' both become the sink name \
                 'eq_{slug}'. PipeWire does not refuse a duplicate — both chains load and \
                 which one you hear is down to startup order. Pick a name that differs by \
                 more than punctuation."
            ));
        }
    }
    Ok(())
}

/// Where `create` writes, and the source `promote` renames.
pub fn draft_path(name: &str) -> PathBuf {
    cwd().join(format!("{name}.draft.conf"))
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

/// Makeup gain is a high shelf whose corner sits below the audible band, so it lifts
/// everything the listener can hear by the amount asked for. Protection of the driver
/// belongs to the high-pass, not to this stage: one job per stage, so a corner low
/// enough to be flat from 30 Hz up is the right one.
pub const PREAMP_FREQ: f64 = 10.0;
pub const PREAMP_Q: f64 = 0.7;

/// Most gain one node may carry.
///
/// PipeWire declares each builtin's `Gain` control with `max = 20.0` and clamps
/// values read from a config against it, logging at info level and otherwise
/// silently. Splitting the makeup gain across enough shelves to stay under that
/// keeps a profile delivering the level it states, on every version. Two shelves
/// carrying half each land within 0.05 dB of one shelf carrying the total, so the
/// split costs nothing in the audible band.
pub const PREAMP_MAX_PER_NODE: f64 = 18.0;

/// Ceiling on the total, as a guard against a mistyped figure rather than a limit
/// anyone should need to reach.
pub const PREAMP_MAX_TOTAL: f64 = 30.0;

/// The sections makeup gain compiles to — empty when none was asked for.
pub fn preamp_bands(boost: f64) -> Vec<Band> {
    if boost <= 0.0 {
        return Vec::new();
    }
    let stages = (boost / PREAMP_MAX_PER_NODE).ceil().max(1.0) as usize;
    let each = (boost / stages as f64 * 10.0).round() / 10.0;
    (0..stages)
        .map(|i| Band {
            freq: PREAMP_FREQ,
            gain: each,
            q: PREAMP_Q,
            kind: "bq_highshelf",
            node: if stages == 1 { "preamp".to_string() } else { format!("preamp{}", i + 1) },
            note: if stages == 1 {
                format!("makeup gain: +{boost} dB across the audible band")
            } else {
                format!(
                    "makeup gain: +{each} dB, stage {} of {stages} (+{boost} dB total;                      one node may carry at most +{PREAMP_MAX_PER_NODE:.0} dB)",
                    i + 1
                )
            },
        })
        .collect()
}

/// How a profile was designed, recorded in its header so the file carries its own
/// assumptions — these get hand-edited, committed and shared, and a reader needs to
/// know what bounded the bands as much as what the bands are.
pub struct Design<'a> {
    pub boost: f64,
    pub class: &'a str,
    pub highpass: Option<f64>,
    pub alignment: Alignment,
    pub target: &'a str,
}

pub fn speaker_class(path: &PathBuf) -> Option<String> {
    field(path, "speaker-class")
}

pub fn highpass(path: &PathBuf) -> Option<f64> {
    field(path, "highpass").and_then(|v| v.split_whitespace().next()?.parse().ok())
}

/// A control value with no trailing-zero noise: these files are read and edited by
/// hand, and `0.541196100146197` carries no more meaning than `0.5412`.
fn ctl(v: f64) -> String {
    let s = format!("{v:.4}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Write a profile config: the graph is `chain` in order, the header is `d`.
pub fn write_profile(name: &str, chain: &[Band], d: &Design, out: &PathBuf) -> Result<()> {
    let slug = slugify(name);
    let mut nodes: Vec<String> = Vec::new();
    let mut links: Vec<String> = Vec::new();
    let mut prev: Option<String> = None;

    let add = |node_name: &str, label: &str, freq: f64, q: f64, gain: f64, comment: &str,
               nodes: &mut Vec<String>, links: &mut Vec<String>, prev: &mut Option<String>| {
        let (freq, q, gain) = (ctl(freq), ctl(q), ctl(gain));
        nodes.push(format!(
            "                    {{\n\
             \x20                       type  = builtin\n\
             \x20                       name  = {node_name}\n\
             \x20                       label = {label}\n\
             \x20                       control = {{\n\
             \x20                           # {comment}\n\
             \x20                           \"Freq\"  = {freq}\n\
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

    for b in chain {
        add(&b.node, b.kind, b.freq, b.q, b.gain, &b.note, &mut nodes, &mut links, &mut prev);
    }

    let eq_bands = chain.iter().filter(|b| b.node.starts_with("eq_band")).count();
    let hp = match d.highpass {
        Some(f) => format!(
            "# highpass: {f:.0}\n\
             # highpass-align: {align}\n\
             #   4th-order, {corner:.2} dB at the corner. Below here the driver moves\n\
             #   without being heard, and protection reacts to the peaks by pulling\n\
             #   everything down — so removing it can leave the rest louder.\n",
            align = d.alignment.name(),
            corner = d.alignment.corner_db(),
        ),
        None => String::new(),
    };
    let header = format!(
        "# {name} — PipeWire filter-chain speaker EQ\n\
         # Auto-generated by pwtune; edit freely and re-measure to refine.\n\
         # Makeup gain (preamp): +{boost} dB   |   {eq_bands} EQ band(s)\n\
         #\n\
         # target-match: {target}\n\
         #   `pwtune install` resolves this to the actual sink node.name (it differs\n\
         #   per machine/GPU/bus, but the monitor/device name is stable). If you\n\
         #   deploy this by hand, replace __TARGET_SINK__ with a name from\n\
         #   `pactl list short sinks`.\n\
         #\n\
         # speaker-class: {class}\n\
         #   Bounds the design: no band is placed below the high-pass corner.\n\
         {hp}\n",
        boost = d.boost,
        target = d.target,
        class = d.class,
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
         \x20               # WirePlumber's shipped suspend rule matches alsa_input.*\n\
         \x20               # and alsa_output.* only, and applies through the ALSA\n\
         \x20               # monitor, which never sees a filter-chain node. Without\n\
         \x20               # this these nodes idle out after 5 s, and some amplifiers\n\
         \x20               # click when the PCM reopens. 0 means never.\n\
         \x20               session.suspend-timeout-seconds = 0\n\
         \x20           }}\n\
         \x20           playback.props = {{\n\
         \x20               node.name   = \"effect_output.eq_{slug}\"\n\
         \x20               node.target = \"__TARGET_SINK__\"\n\
         \x20               audio.channels = 2\n\
         \x20               audio.position = [ FL FR ]\n\
         \x20               session.suspend-timeout-seconds = 0\n\
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::{highpass_bands, Alignment, Band};

    fn chain() -> Vec<Band> {
        let mut c = highpass_bands(100.0, Alignment::Butterworth);
        c.extend(preamp_bands(6.0));
        c.push(Band {
            freq: 150.0,
            gain: 4.5,
            q: 1.2,
            kind: "bq_peaking",
            node: "eq_band1".into(),
            note: "correct toward target at 150 Hz".into(),
        });
        c.push(Band {
            freq: 3500.0,
            gain: -2.0,
            q: 1.2,
            kind: "bq_peaking",
            node: "eq_band2".into(),
            note: "correct toward target at 3500 Hz".into(),
        });
        c
    }

    /// Trailing zeros go, digits never do. The formatted value always carries a
    /// decimal point, which is what stops the trim from eating the integer part.
    #[test]
    fn control_values_keep_every_digit() {
        assert_eq!(ctl(10.0), "10");
        assert_eq!(ctl(100.0), "100");
        assert_eq!(ctl(20000.0), "20000");
        assert_eq!(ctl(0.0), "0");
        assert_eq!(ctl(1.2), "1.2");
        assert_eq!(ctl(-2.0), "-2");
        assert_eq!(ctl(4.5), "4.5");
        assert_eq!(ctl(0.541_196_100_146_197), "0.5412");
    }

    fn write_to_temp(tag: &str) -> PathBuf {
        let out = std::env::temp_dir()
            .join(format!("pwtune-test-{}-{tag}.conf", std::process::id()));
        let d = Design { boost: 6.0, class: "laptop-woofer", highpass: Some(100.0),
            alignment: Alignment::Butterworth, target: "X1 Speaker" };
        write_profile("desk", &chain(), &d, &out).expect("write");
        out
    }

    #[test]
    fn header_fields_round_trip() {
        let out = write_to_temp("roundtrip");
        assert_eq!(target_match(&out).as_deref(), Some("X1 Speaker"));
        assert_eq!(speaker_class(&out).as_deref(), Some("laptop-woofer"));
        assert_eq!(highpass(&out), Some(100.0));
        assert_eq!(input_node(&out).as_deref(), Some("effect_input.eq_desk"));
        let _ = fs::remove_file(out);
    }

    /// Every node after the first is linked to the one before it, or the graph has a
    /// dangling stage and PipeWire loads a chain that plays silence.
    #[test]
    fn every_stage_is_linked_in_order() {
        let out = write_to_temp("links");
        let txt = fs::read_to_string(&out).unwrap();
        let c = chain();
        for b in &c {
            assert!(txt.contains(&format!("name  = {}", b.node)), "missing node {}", b.node);
            assert!(txt.contains(&b.note), "missing note for {}", b.node);
        }
        assert_eq!(txt.matches("output = ").count(), c.len() - 1, "one link per hop");
        for w in c.windows(2) {
            let link = format!("{{ output = \"{}:Out\" input = \"{}:In\" }}", w[0].node, w[1].node);
            assert!(txt.contains(&link), "missing link {link}");
        }
        let _ = fs::remove_file(out);
    }

    /// A profile that declares no floor says so, rather than omitting the field and
    /// leaving a reader to guess whether it was considered.
    #[test]
    fn no_high_pass_means_no_high_pass_line() {
        let out = std::env::temp_dir().join(format!("pwtune-test-{}-nohp.conf", std::process::id()));
        let d = Design { boost: 0.0, class: "fullrange", highpass: None,
            alignment: Alignment::Butterworth, target: "Monitors" };
        write_profile("hifi", &[], &d, &out).unwrap();
        let txt = fs::read_to_string(&out).unwrap();
        assert!(!txt.contains("# highpass:"));
        assert_eq!(speaker_class(&out).as_deref(), Some("fullrange"));
        let _ = fs::remove_file(out);
    }
}
