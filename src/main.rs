//! speaker-eq — measure any speaker with any mic, build a PipeWire filter-chain
//! EQ profile, and install/remove profiles. Rust port of the original Python tool.
mod dsp;
mod profile;
mod system;
mod ui;

use anyhow::Result;
use clap::{Parser, Subcommand};
use profile::{
    calibrated_dir, calibrated_names, conf_d, description, draft_names, drafts_dir, find_profile,
    input_node, profile_names, target_match, write_profile, Tier, DEPLOY_PREFIX,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use system::{
    deployed_eq_configs, detect_mic, detect_speaker, get_default_sink, get_devices, play_and_record,
    resolve_sink, restart_pipewire, set_default_sink, Device, Kind,
};
use ui::{ask, err, info, next_steps, ok, pick, pick_sections, run_cmd, slugify, warn};

#[derive(Parser)]
#[command(name = "speaker-eq", about = "Measure, build, and install PipeWire speaker EQ profiles.")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Play a sweep and print the frequency response; no changes.
    Measure {
        #[arg(long)]
        speaker: Option<String>,
        #[arg(long)]
        mic: Option<String>,
        #[arg(long, default_value_t = dsp::DEFAULT_ITERATIONS)]
        iterations: usize,
    },
    /// Measure + generate a starting profile in drafts/.
    Create {
        name: Option<String>,
        #[arg(long)]
        boost: Option<f64>,
        #[arg(long = "target-match")]
        target_match: Option<String>,
        #[arg(long)]
        speaker: Option<String>,
        #[arg(long)]
        mic: Option<String>,
        #[arg(long, default_value_t = dsp::DEFAULT_ITERATIONS)]
        iterations: usize,
    },
    /// List profiles (draft + shipped) and their install state.
    List,
    /// Install a profile as an EQ output.
    Install {
        name: Option<String>,
        #[arg(long)]
        speaker: Option<String>,
    },
    /// Open a draft profile in $EDITOR (calibrated ones are frozen).
    Edit { name: Option<String> },
    /// Delete a draft profile (calibrated ones are frozen).
    Delete { name: Option<String> },
    /// Remove a deployed EQ.
    Uninstall { name: Option<String> },
    /// Finalize a draft: move it into calibrated/ (shipped, frozen).
    Promote { name: Option<String> },
    /// (dev) Analyze a sweep+recording pair and print the response; for validation.
    #[command(hide = true)]
    DevAnalyze { sweep: String, rec: String },
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap_or("")
}

fn tmpdir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("speaker-eq-{}", std::process::id()));
    let _ = fs::create_dir_all(&d);
    d
}

fn average(rows: &[Vec<f64>]) -> Vec<f64> {
    let n = rows.len() as f64;
    let len = rows[0].len();
    (0..len).map(|k| rows.iter().map(|r| r[k]).sum::<f64>() / n).collect()
}

fn choose_device(kind: &str, devs: &[Device], detected: Option<usize>) -> usize {
    if devs.is_empty() {
        err(&format!("No {kind}s found. Is PipeWire running?"));
        std::process::exit(1);
    }
    eprintln!("\nAvailable {kind}s:");
    for (i, d) in devs.iter().enumerate() {
        let label = if !d.nick.is_empty() { &d.nick } else if !d.desc.is_empty() { &d.desc } else { &d.name };
        let mark = if Some(i) == detected { "   ← detected (press Enter)" } else { "" };
        eprintln!("  {}) {label}{mark}", i + 1);
        eprintln!("       {}", d.name);
    }
    let def = detected.map(|i| (i + 1).to_string()).unwrap_or_default();
    loop {
        let ans = ask(&format!("Pick a {kind}"), &def);
        if let Ok(n) = ans.parse::<usize>() {
            if n >= 1 && n <= devs.len() {
                return n - 1;
            }
        }
        eprintln!("  Please enter a number from the list.");
    }
}

/// (speaker_name, mic_name, speaker_nick), guiding the user unless given on the CLI.
fn resolve_devices(speaker: Option<String>, mic: Option<String>) -> (String, String, String) {
    let (sname, snick) = match speaker {
        Some(x) => (x, String::new()),
        None => {
            let sinks: Vec<Device> = get_devices("sinks")
                .into_iter()
                .filter(|d| !d.name.starts_with("effect_input.") && !d.name.starts_with("effect_output."))
                .collect();
            let det = detect_speaker(&sinks);
            let i = choose_device("speaker", &sinks, det);
            (sinks[i].name.clone(), sinks[i].nick.clone())
        }
    };
    let mname = match mic {
        Some(x) => x,
        None => {
            let srcs: Vec<Device> = get_devices("sources")
                .into_iter()
                .filter(|d| !d.name.to_lowercase().contains("monitor"))
                .collect();
            let det = detect_mic(&srcs);
            let i = choose_device("microphone", &srcs, det);
            srcs[i].name.clone()
        }
    };
    info(&format!("Speaker: {}", if snick.is_empty() { &sname } else { &snick }));
    info(&format!("Mic:     {mname}"));
    (sname, mname, snick)
}

fn measure_passes(sp: &str, mc: &str, iters: usize, tmp: &Path, keep: bool) -> (Vec<f64>, Vec<Vec<f64>>) {
    let sweep = tmp.join("sweep.wav");
    dsp::write_wav(s(&sweep), &dsp::gen_sweep_i16()).expect("write sweep");
    let mut freqs = Vec::new();
    let mut rows = Vec::new();
    for i in 1..=iters {
        println!("--- pass {i}/{iters} ---");
        let rec = tmp.join(format!("rec{i}.wav"));
        if let Err(e) = play_and_record(s(&sweep), s(&rec), sp, mc) {
            err(&format!("record failed: {e}"));
            continue;
        }
        match dsp::analyze_response(s(&sweep), s(&rec)) {
            Ok(Some((f, db))) => {
                freqs = f;
                if keep {
                    dsp::print_response(&freqs, &db, &format!("pass {i} (relative)"));
                }
                rows.push(db);
            }
            _ => {}
        }
    }
    (freqs, rows)
}

fn cmd_measure(speaker: Option<String>, mic: Option<String>, iters: usize) -> Result<()> {
    let (sp, mc, _) = resolve_devices(speaker, mic);
    println!("\nKeep the room quiet. Playing sweeps...\n");
    let tmp = tmpdir();
    let (freqs, rows) = measure_passes(&sp, &mc, iters, &tmp, true);
    if rows.len() > 1 {
        dsp::print_response(&freqs, &average(&rows), &format!("AVERAGE of {} passes", rows.len()));
    }
    let _ = fs::remove_dir_all(&tmp);
    println!("\nMeasurement complete — nothing changed.");
    next_steps(&[
        ("Build an EQ from a speaker like this", run_cmd("create", "")),
        ("Install a profile you already have", run_cmd("install", "")),
    ]);
    Ok(())
}

fn cmd_create(
    name: Option<String>,
    boost: Option<f64>,
    tmatch: Option<String>,
    speaker: Option<String>,
    mic: Option<String>,
    iters: usize,
) -> Result<()> {
    let (sp, mc, nick) = resolve_devices(speaker, mic);
    let name = name.unwrap_or_else(|| {
        let def = if nick.is_empty() { "my-speaker".to_string() } else { slugify(&nick) };
        ask("\nName this profile", &def)
    });
    if name.is_empty() {
        warn("No name given — aborting.");
        return Ok(());
    }
    let boost = boost.unwrap_or_else(|| ask("Makeup gain in dB (louder overall; 0 = none)", "0").parse().unwrap_or(0.0));
    let target = tmatch.unwrap_or_else(|| if !nick.is_empty() { nick.clone() } else { sp.clone() });
    info(&format!("Profile '{name}': target-match='{target}', makeup +{boost} dB"));
    println!("\nKeep the room quiet. Measuring to design a starting EQ...\n");
    let tmp = tmpdir();
    let (freqs, rows) = measure_passes(&sp, &mc, iters, &tmp, false);
    let _ = fs::remove_dir_all(&tmp);
    if rows.is_empty() {
        err("No usable measurements.");
        std::process::exit(1);
    }
    let avg = average(&rows);
    dsp::print_response(&freqs, &avg, "measured (average)");
    let bands = dsp::design_eq(&freqs, &avg);
    let out = drafts_dir().join(format!("{name}.conf"));
    write_profile(&name, &bands, boost, &target, &out)?;
    ok(&format!("Wrote draft {}  ({} bands, +{boost} dB makeup)", out.display(), bands.len()));
    println!("  A starting point: the low/low-mid correction is measured; the top end");
    println!("  is a guess (the mic barely hears it) — refine it by ear.");
    next_steps(&[
        ("Hear it", run_cmd("install", &name)),
        ("Tweak it", format!("{}, then re-run {}", run_cmd("edit", &name), run_cmd("install", &name))),
        ("Finalize it (ship it)", run_cmd("promote", &name)),
    ]);
    Ok(())
}

fn cmd_list() -> Result<()> {
    let names = profile_names();
    if names.is_empty() {
        warn("No profiles yet — make one with `create`.");
        return Ok(());
    }
    let default = get_default_sink();
    println!();
    for p in &names {
        let (path, tier) = find_profile(p).unwrap();
        let desc = description(&path);
        let connected = target_match(&path).map(|m| !resolve_sink(&m).is_empty()).unwrap_or(false);
        let node = input_node(&path);
        let mut tags = vec![if tier == Tier::Shipped { "shipped" } else { "draft" }.to_string()];
        if conf_d().join(format!("{DEPLOY_PREFIX}{p}.conf")).is_file() {
            tags.push("installed".into());
        }
        if node.as_deref() == Some(default.as_str()) {
            tags.push("active output".into());
        }
        let dot = if connected { "●" } else { "○" };
        println!("  {dot} {p:<22}{desc:<30}[{}]", tags.join(" · "));
    }
    println!("\n  ● speaker connected now    ○ not connected");
    println!("  draft = editable work in progress   ·   shipped = finalized (frozen)");
    next_steps(&[
        ("Install one", format!("{}  (pick from the list)", run_cmd("install", ""))),
        ("Create a new one", run_cmd("create", "")),
    ]);
    Ok(())
}

fn cmd_install(name: Option<String>, speaker_override: Option<String>) -> Result<()> {
    let profs = profile_names();
    if profs.is_empty() {
        err("No profiles yet — make one with `create`.");
        std::process::exit(1);
    }
    let mut default: Option<String> = None;
    for p in &profs {
        if let Some((path, _)) = find_profile(p) {
            if let Some(m) = target_match(&path) {
                if !resolve_sink(&m).is_empty() {
                    info(&format!("Connected speaker detected → default profile '{p}'"));
                    default = Some(p.clone());
                    break;
                }
            }
        }
    }
    let sections = [
        ("Drafts (work in progress)", draft_names()),
        ("Calibrated (promoted / shipped)", calibrated_names()),
    ];
    let name = match name {
        Some(n) => n,
        None => match pick_sections("Pick a profile to install", &sections, default.as_deref().or(Some(&profs[0]))) {
            Some(n) => n,
            None => {
                warn("Nothing selected.");
                return Ok(());
            }
        },
    };
    let (path, tier) = match find_profile(&name) {
        Some(x) => x,
        None => {
            err(&format!("Unknown profile '{name}'. Available: {}", profs.join(", ")));
            std::process::exit(1);
        }
    };
    let m = match target_match(&path) {
        Some(m) => m,
        None => {
            err(&format!("Profile '{name}' has no '# target-match:' line."));
            std::process::exit(1);
        }
    };
    let pat = speaker_override.clone().unwrap_or(m.clone());
    let hits = resolve_sink(&pat);
    if hits.is_empty() {
        err(&format!("No sink matches '{pat}' — is that speaker/monitor connected?"));
        println!("  Connected sinks:");
        for d in get_devices("sinks") {
            println!("    {}", d.name);
        }
        std::process::exit(1);
    }
    let target = if hits.len() == 1 {
        hits[0].clone()
    } else {
        match pick_sections(&format!("Target sink for '{name}'"), &[("Sinks", hits.clone())], Some(&hits[0])) {
            Some(t) => t,
            None => {
                warn("No target chosen.");
                return Ok(());
            }
        }
    };
    let text = fs::read_to_string(&path)?.replace("__TARGET_SINK__", &target);
    fs::create_dir_all(conf_d())?;
    let dest = conf_d().join(format!("{DEPLOY_PREFIX}{name}.conf"));
    fs::write(&dest, text)?;
    ok(&format!("Installed '{name}' → {target}"));
    info("Restarting PipeWire...");
    restart_pipewire();
    if let Some(node) = input_node(&path) {
        if set_default_sink(&node) {
            ok(&format!("Default output set to {node}"));
        } else {
            warn("Loaded, but couldn't set default — pick it in your sound settings.");
        }
    }
    println!("\n  '{name}' is now your output — play something to hear it.");
    let mut steps: Vec<(&str, String)> = Vec::new();
    if tier == Tier::Draft {
        steps.push(("Re-tune", format!("{}, then re-run {}", run_cmd("edit", &name), run_cmd("install", &name))));
    }
    steps.push(("Turn it off", run_cmd("uninstall", &name)));
    if tier == Tier::Draft {
        steps.push(("Finalize it (ship it)", run_cmd("promote", &name)));
    }
    next_steps(&steps);
    Ok(())
}

fn cmd_edit(name: Option<String>) -> Result<()> {
    let drafts = draft_names();
    if drafts.is_empty() {
        err("No draft profiles to edit — make one with `create`.");
        return Ok(());
    }
    let name = name.or_else(|| pick("Pick a draft to edit", &drafts, Some(&drafts[0]))).unwrap_or_default();
    if name.is_empty() {
        warn("Nothing selected.");
        return Ok(());
    }
    match find_profile(&name) {
        Some((path, Tier::Draft)) => {
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nano".into());
            let mut parts = editor.split_whitespace();
            let prog = parts.next().unwrap_or("nano");
            info(&format!("Opening {} in {editor} …", path.display()));
            let _ = Command::new(prog).args(parts).arg(&path).status();
            next_steps(&[("Try your changes", run_cmd("install", &name))]);
        }
        Some((_, Tier::Shipped)) => {
            err(&format!("'{name}' is finalized (in calibrated/) and frozen — edit is for drafts only."));
            println!("  To change it, create a new draft (re-measure) and promote again.");
        }
        None => err(&format!("Unknown profile '{name}'. Drafts: {}", drafts.join(", "))),
    }
    Ok(())
}

fn cmd_delete(name: Option<String>) -> Result<()> {
    let drafts = draft_names();
    if drafts.is_empty() {
        err("No draft profiles to delete.");
        return Ok(());
    }
    let name = name.or_else(|| pick("Pick a draft to delete", &drafts, Some(&drafts[0]))).unwrap_or_default();
    if name.is_empty() {
        warn("Nothing selected.");
        return Ok(());
    }
    match find_profile(&name) {
        Some((path, Tier::Draft)) => {
            if conf_d().join(format!("{DEPLOY_PREFIX}{name}.conf")).is_file() {
                warn(&format!("'{name}' is currently installed — uninstall it first if you don't want it playing."));
            }
            if !ask(&format!("Delete drafts/{name}.conf?"), "n").to_lowercase().starts_with('y') {
                warn("Kept.");
                return Ok(());
            }
            fs::remove_file(&path)?;
            ok(&format!("Deleted drafts/{name}.conf"));
        }
        Some((_, Tier::Shipped)) => {
            err(&format!("'{name}' is finalized (in calibrated/) and frozen — delete is for drafts only."));
        }
        None => err(&format!("Unknown profile '{name}'. Drafts: {}", drafts.join(", "))),
    }
    Ok(())
}

fn cmd_uninstall(name: Option<String>) -> Result<()> {
    let items = deployed_eq_configs();
    if items.is_empty() {
        warn(&format!("No EQ configs are deployed in {}.", conf_d().display()));
        return Ok(());
    }
    let mut buckets: [(&str, Vec<String>); 4] = [
        ("Drafts (work in progress)", vec![]),
        ("Calibrated (promoted / shipped)", vec![]),
        ("Orphans (installed, but the profile is gone from the repo)", vec![]),
        ("Other EQ configs on this system (not from this tool)", vec![]),
    ];
    let mut label_path: Vec<(String, PathBuf)> = Vec::new();
    for (label, path, kind) in items {
        let b = match kind {
            Kind::Draft => 0,
            Kind::Shipped => 1,
            Kind::Orphan => 2,
            Kind::External => 3,
        };
        buckets[b].1.push(label.clone());
        label_path.push((label, path));
    }
    let sections: Vec<(&str, Vec<String>)> = buckets.iter().map(|(h, v)| (*h, v.clone())).collect();
    let order: Vec<String> = buckets.iter().flat_map(|(_, v)| v.clone()).collect();
    let name = match name {
        Some(n) => n,
        None => match pick_sections("Pick an EQ to uninstall", &sections, order.first().map(|x| x.as_str())) {
            Some(n) => n,
            None => {
                warn("Nothing selected.");
                return Ok(());
            }
        },
    };
    let path = label_path
        .iter()
        .find(|(l, _)| *l == name)
        .map(|(_, p)| p.clone())
        .unwrap_or_else(|| conf_d().join(format!("{DEPLOY_PREFIX}{name}.conf")));
    if !path.is_file() {
        err(&format!("'{name}' is not installed."));
        return Ok(());
    }
    fs::remove_file(&path)?;
    ok(&format!("Removed {}", path.display()));
    info("Restarting PipeWire...");
    restart_pipewire();
    println!("\n  EQ removed — audio goes straight to the speaker again.");
    next_steps(&[
        ("See all profiles", run_cmd("list", "")),
        ("Install one", run_cmd("install", "")),
    ]);
    Ok(())
}

fn cmd_promote(name: Option<String>) -> Result<()> {
    let drafts = draft_names();
    if drafts.is_empty() {
        err("No drafts to finalize — make one with `create`.");
        return Ok(());
    }
    let name = name.or_else(|| pick("Pick a draft to finalize", &drafts, Some(&drafts[0]))).unwrap_or_default();
    if name.is_empty() {
        warn("Nothing selected.");
        return Ok(());
    }
    let src = drafts_dir().join(format!("{name}.conf"));
    if !src.is_file() {
        if calibrated_dir().join(format!("{name}.conf")).is_file() {
            warn(&format!("'{name}' is already finalized (in calibrated/)."));
        } else {
            err(&format!("No draft named '{name}'. Drafts: {}", drafts.join(", ")));
        }
        return Ok(());
    }
    fs::create_dir_all(calibrated_dir())?;
    let dest = calibrated_dir().join(format!("{name}.conf"));
    if dest.exists() && !ask(&format!("calibrated/{name}.conf exists — overwrite?"), "n").to_lowercase().starts_with('y') {
        warn("Kept the existing one; draft left in place.");
        return Ok(());
    }
    fs::rename(&src, &dest)?;
    ok(&format!("Finalized: drafts/{name}.conf → calibrated/{name}.conf"));
    println!("  It's frozen now (edit/delete won't touch it) and ships with the repo.");
    next_steps(&[
        ("Publish it", "git add calibrated/ && commit && push".to_string()),
        ("Deploy on a machine", "in ReinstallScripts: just eq-import, then just speaker-eq".to_string()),
    ]);
    Ok(())
}

fn cmd_dev_analyze(sweep: &str, rec: &str) -> Result<()> {
    match dsp::analyze_response(sweep, rec)? {
        Some((freqs, db)) => {
            for &cf in &[63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0] {
                println!("{cf} {:.4}", dsp::value_at(&freqs, &db, cf));
            }
        }
        None => err("recording too short"),
    }
    Ok(())
}

fn main() -> Result<()> {
    let _ = ctrlc::set_handler(|| {
        eprintln!("\nCancelled.");
        std::process::exit(0);
    });
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Measure { speaker, mic, iterations } => cmd_measure(speaker, mic, iterations),
        Cmd::Create { name, boost, target_match, speaker, mic, iterations } => {
            cmd_create(name, boost, target_match, speaker, mic, iterations)
        }
        Cmd::List => cmd_list(),
        Cmd::Install { name, speaker } => cmd_install(name, speaker),
        Cmd::Edit { name } => cmd_edit(name),
        Cmd::Delete { name } => cmd_delete(name),
        Cmd::Uninstall { name } => cmd_uninstall(name),
        Cmd::Promote { name } => cmd_promote(name),
        Cmd::DevAnalyze { sweep, rec } => cmd_dev_analyze(&sweep, &rec),
    }
}
