#!/usr/bin/env python3
"""
pipewire-speaker-calibration — measure any speaker with any mic, build a PipeWire
filter-chain EQ profile, and install/remove profiles.

This is the tool. The justfile is only a thin set of shortcuts over these
subcommands, so everything works with plain `python3 speaker-eq.py <cmd>` too.

Subcommands:
  measure               Play a sweep, record it, print the frequency response. No changes.
  create   <name>       measure + auto-generate a *starting* EQ profile -> profiles/<name>.conf
  list                  List profiles and show which are installed / which is the default output
  install  [name]       Resolve the profile's target sink, deploy it, make it the default output
  uninstall [name]      Remove an installed profile
  promote  <name>       Copy profiles/<name>.conf into a sibling ReinstallScripts checkout

Measurement honesty: auto-generated EQ is a STARTING POINT, not a finished tuning.
Cheap mics (webcams, headset mics) have a noise floor, comb filtering off nearby
surfaces, and mains hum — trust the low/low-mid bands, use your ears up top, and
iterate measure -> edit profiles/<name>.conf -> re-measure.
"""

import argparse
import os
import re
import subprocess
import sys
import tempfile
import time

REPO_DIR = os.path.dirname(os.path.abspath(__file__))
PROFILES_DIR = os.path.join(REPO_DIR, "profiles")
CONF_D = os.path.expanduser("~/.config/pipewire/pipewire.conf.d")
DEPLOY_PREFIX = "eqlab-"           # so lab installs never clash with other tools' files
SIBLING_RIS_ASSETS = os.path.normpath(
    os.path.join(REPO_DIR, "..", "ReinstallScripts", "Linux", "assets", "speaker-eq"))

RATE = 48000
DURATION = 3
SWEEP_LOW = 50
SWEEP_HIGH = 20000
SETTLE = 0.5
DEFAULT_ITERATIONS = 3

# Target: mild bass-boost curve (flat sounds thin on small speakers). dB above flat.
TARGET_CURVE = {50: 6, 100: 5, 200: 4, 400: 2, 800: 0, 1500: 0, 2500: -1, 4000: 0, 8000: 0, 16000: -1}

C = {"blue": "\033[1;34m", "green": "\033[1;32m", "yellow": "\033[1;33m", "red": "\033[1;31m", "off": "\033[0m"}
def info(m): print(f"{C['blue']}▸ {m}{C['off']}")
def ok(m):   print(f"{C['green']}✓ {m}{C['off']}")
def warn(m): print(f"{C['yellow']}⚠ {m}{C['off']}")
def err(m):  print(f"{C['red']}✗ {m}{C['off']}", file=sys.stderr)


# ─────────────────────────── device discovery ───────────────────────────────

def get_pactl_devices(kind):
    """List sinks/sources as [{name, desc, nick}] (nick = node.nick / EDID name)."""
    out = subprocess.run(["pactl", "list", kind], capture_output=True, text=True).stdout
    devices, cur = [], {}
    for line in out.splitlines():
        s = line.strip()
        if s.startswith("Name:"):
            if cur.get("name"):
                devices.append(cur)
            cur = {"name": s.split(":", 1)[1].strip(), "desc": "", "nick": ""}
        elif s.startswith("Description:"):
            cur["desc"] = s.split(":", 1)[1].strip()
        elif s.startswith("node.nick"):
            cur["nick"] = s.split("=", 1)[1].strip().strip('"')
    if cur.get("name"):
        devices.append(cur)
    return devices


def detect_speaker_sink(devices):
    exclude = re.compile(r'effect_input|effect_output|monitor', re.I)
    for d in devices:
        if re.search(r'speaker', d["name"] + d["desc"] + d["nick"], re.I) and not exclude.search(d["name"]):
            return d
    for d in devices:
        if re.search(r'sof|hda|analog|hdmi', d["name"], re.I) and not exclude.search(d["name"]):
            return d
    return None


def detect_mic_source(devices):
    exclude = re.compile(r'monitor|effect_', re.I)
    for d in devices:
        text = d["name"] + d["desc"] + d["nick"]
        if re.search(r'mic', text, re.I) and not re.search(r'headset', text, re.I) and not exclude.search(d["name"]):
            return d
    for d in devices:
        if not exclude.search(d["name"]):
            return d
    return None


def prompt_device_choice(kind, devices):
    print(f"\n  Available {kind}:", file=sys.stderr)
    for i, d in enumerate(devices, 1):
        label = d["nick"] or d["desc"] or d["name"]
        print(f"    {i}. {label}  ({d['name']})", file=sys.stderr)
    while True:
        try:
            idx = int(input(f"\n  Enter number (1-{len(devices)}): ").strip()) - 1
            if 0 <= idx < len(devices):
                return devices[idx]
        except (ValueError, EOFError):
            pass
        print("  Invalid choice, try again.", file=sys.stderr)


def resolve_devices(speaker_arg, mic_arg):
    """Resolve (speaker_sink, mic_source, speaker_nick) from args, auto-detect, or a prompt."""
    sinks = get_pactl_devices("sinks")
    sources = [d for d in get_pactl_devices("sources") if "monitor" not in d["name"].lower()]

    if speaker_arg:
        speaker = {"name": speaker_arg, "desc": speaker_arg, "nick": ""}
    else:
        speaker = detect_speaker_sink(sinks)
        if speaker:
            info(f"Speaker: {speaker['nick'] or speaker['desc']}  ({speaker['name']})")
        else:
            if not sinks:
                err("No audio sinks found. Is PipeWire running?"); sys.exit(1)
            speaker = prompt_device_choice("speakers", sinks)

    if mic_arg:
        mic = {"name": mic_arg, "desc": mic_arg, "nick": ""}
    else:
        mic = detect_mic_source(sources)
        if mic:
            info(f"Mic:     {mic['nick'] or mic['desc']}  ({mic['name']})")
        else:
            if not sources:
                err("No audio sources found."); sys.exit(1)
            mic = prompt_device_choice("microphones", sources)

    return speaker["name"], mic["name"], speaker.get("nick", "")


def resolve_sink(pattern):
    """Sink node.name(s) whose full `pactl list sinks` block matches pattern
    (case-insensitive). Matches node.nick / alsa.name too (portable EDID name),
    and excludes our own effect_* virtual sinks so a profile can't target itself."""
    out = subprocess.run(["pactl", "list", "sinks"], capture_output=True, text=True).stdout
    hits = []
    for chunk in re.split(r'(?m)^Sink #', out):
        if not chunk.strip():
            continue
        if re.search(pattern, chunk, re.I):
            m = re.search(r'Name:\s*(\S+)', chunk)
            if m and not m.group(1).startswith(("effect_input.", "effect_output.")):
                if m.group(1) not in hits:
                    hits.append(m.group(1))
    return hits


def sink_nick(name):
    """node.nick (EDID/monitor name) for a given sink node.name, or ''."""
    for d in get_pactl_devices("sinks"):
        if d["name"] == name:
            return d.get("nick", "")
    return ""


# ─────────────────────────── measurement ────────────────────────────────────

def _np():
    import numpy as np
    from scipy import signal  # noqa: F401  (imported for side effects / availability)
    return np


def generate_sweep(filename):
    np = _np()
    from scipy.io import wavfile
    n_settle = int(SETTLE * RATE)
    n_sweep = int(DURATION * RATE)
    t = np.linspace(0, DURATION, n_sweep, endpoint=False)
    phase = 2 * np.pi * SWEEP_LOW * DURATION / np.log(SWEEP_HIGH / SWEEP_LOW) * \
        (np.exp(t / DURATION * np.log(SWEEP_HIGH / SWEEP_LOW)) - 1)
    sweep = 0.7 * np.sin(phase)
    audio = np.concatenate([np.zeros(n_settle), sweep, np.zeros(n_settle)])
    wavfile.write(filename, RATE, (audio * 32767).astype(np.int16))
    return n_settle, n_sweep


def play_and_record(sweep_file, rec_file, speaker, mic):
    subprocess.run(["pactl", "set-default-source", mic], capture_output=True)
    rec = subprocess.Popen(
        ["parecord", "--format=s16le", f"--rate={RATE}", "--channels=1",
         f"--device={mic}", "--file-format=wav", rec_file],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(1)
    subprocess.run(["paplay", f"--device={speaker}", sweep_file],
                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(1.5)
    rec.terminate(); rec.wait()


def analyze_response(sweep_file, rec_file, n_settle, n_sweep):
    np = _np()
    from scipy import signal
    from scipy.io import wavfile
    from scipy.ndimage import uniform_filter1d
    _, sweep_data = wavfile.read(sweep_file)
    _, rec_data = wavfile.read(rec_file)
    sweep_data = sweep_data.astype(np.float64) / 32768.0
    if rec_data.ndim > 1:
        rec_data = rec_data[:, 0]
    rec_data = rec_data.astype(np.float64) / 32768.0
    start = n_settle + int(0.1 * RATE)
    end = n_settle + n_sweep - int(0.1 * RATE)
    if len(rec_data) < end:
        end = min(end, len(rec_data) - int(0.1 * RATE))
        if end <= start:
            err("Recording too short!"); return None, None
    ref, rec = sweep_data[start:end], rec_data[start:end]
    nperseg = min(8192, len(ref) // 4)
    f_ref, psd_ref = signal.welch(ref, fs=RATE, nperseg=nperseg)
    _, psd_rec = signal.welch(rec, fs=RATE, nperseg=nperseg)
    eps = 1e-12
    response_db = 10 * np.log10((psd_rec + eps) / (psd_ref + eps))
    return f_ref, uniform_filter1d(response_db, size=15)


def compute_target(freqs):
    np = _np()
    tf = sorted(TARGET_CURVE)
    return np.interp(freqs, tf, [TARGET_CURVE[f] for f in tf])


def design_eq(freqs, measured_db):
    np = _np()
    error_db = compute_target(freqs) - measured_db
    ref_mask = (freqs >= 500) & (freqs <= 2000)
    if ref_mask.any():
        error_db = error_db - np.mean(error_db[ref_mask])
    bands = []
    for ef in [80, 150, 300, 500, 1000, 2000, 3500, 6000, 10000]:
        idx = np.argmin(np.abs(freqs - ef))
        correction = float(np.clip(error_db[idx], -6, 8))
        if abs(correction) > 0.5:
            if ef <= 120:
                btype, q = "bq_lowshelf", 0.6
            elif ef >= 8000:
                btype, q = "bq_highshelf", 0.7
            else:
                btype, q = "bq_peaking", 1.2
            bands.append({"freq": ef, "gain": round(correction, 1), "q": q, "type": btype})
    return bands


def print_response(freqs, db, label):
    np = _np()
    print(f"\n  {label}:")
    for cf in [63, 125, 250, 500, 1000, 2000, 4000, 8000]:
        val = db[int(np.argmin(np.abs(freqs - cf)))]
        bar = "█" * max(0, min(int(val + 20), 50))
        note = "   (mic-dependent up here — trust your ears)" if cf >= 2000 else ""
        print(f"  {cf:>5} Hz: {val:>+6.1f} dB  {bar}{note}")


# ─────────────────────────── profile authoring ──────────────────────────────

def slugify(name):
    return re.sub(r'[^a-z0-9]+', '_', name.lower()).strip('_')


def write_profile(name, bands, boost, target_match, out_path):
    slug = slugify(name)
    nodes, links, prev = [], [], None

    def add(node_name, label, freq, q, gain, comment):
        nonlocal prev
        nodes.append(
            f'                    {{\n'
            f'                        type  = builtin\n'
            f'                        name  = {node_name}\n'
            f'                        label = {label}\n'
            f'                        control = {{\n'
            f'                            # {comment}\n'
            f'                            "Freq"  = {freq:.1f}\n'
            f'                            "Q"     = {q}\n'
            f'                            "Gain"  = {gain}\n'
            f'                        }}\n'
            f'                    }}')
        if prev:
            links.append(f'                    {{ output = "{prev}:Out" input = "{node_name}:In" }}')
        prev = node_name

    if boost and boost > 0:
        half = round(boost / 2.0, 1)
        add("preamp1", "bq_lowshelf", 20000.0, 0.7, half, f"Makeup gain: +{half} dB (part 1 of +{boost} dB total)")
        add("preamp2", "bq_highshelf", 20.0, 0.7, half, f"Makeup gain: +{half} dB (part 2 of +{boost} dB total)")

    for i, b in enumerate(bands, 1):
        add(f"eq_band{i}", b["type"], float(b["freq"]), b["q"], b["gain"],
            f"auto: correct toward target at {b['freq']} Hz")

    header = (
        f"# {name} — PipeWire filter-chain speaker EQ\n"
        f"# Auto-generated by speaker-eq.py; edit freely and re-measure to refine.\n"
        f"# Makeup gain (preamp): +{boost or 0} dB   |   {len(bands)} EQ band(s)\n"
        f"#\n"
        f"# target-match: {target_match}\n"
        f"#   `speaker-eq.py install` resolves this to the actual sink node.name (it\n"
        f"#   differs per machine/GPU/bus, but the monitor/device name is stable). If\n"
        f"#   you deploy this by hand, replace __TARGET_SINK__ with a name from\n"
        f"#   `pactl list short sinks`.\n\n")

    graph = header + f"""context.modules = [
    {{ name = libpipewire-module-filter-chain
        args = {{
            node.description = "{name}"
            media.name        = "{name}"
            filter.graph = {{
                nodes = [
{chr(10).join(nodes)}
                ]
                links = [
{chr(10).join(links)}
                ]
            }}
            capture.props = {{
                node.name   = "effect_input.eq_{slug}"
                media.class = Audio/Sink
                audio.channels = 2
                audio.position = [ FL FR ]
            }}
            playback.props = {{
                node.name   = "effect_output.eq_{slug}"
                node.target = "__TARGET_SINK__"
                audio.channels = 2
                audio.position = [ FL FR ]
            }}
        }}
    }}
]
"""
    os.makedirs(os.path.dirname(out_path), exist_ok=True)
    with open(out_path, "w") as f:
        f.write(graph)


def profile_field(path, field):
    """Read a `# field: value` header line (e.g. target-match) from a profile."""
    with open(path) as f:
        for line in f:
            m = re.match(rf'^#\s*{re.escape(field)}:\s*(.+?)\s*$', line)
            if m:
                return m.group(1)
    return None


def profile_input_node(path):
    with open(path) as f:
        m = re.search(r'effect_input\.[A-Za-z0-9_]+', f.read())
    return m.group(0) if m else None


# ─────────────────────────── deploy / manage ────────────────────────────────

def restart_pipewire():
    subprocess.run(["systemctl", "--user", "restart", "pipewire", "pipewire-pulse"], capture_output=True)
    time.sleep(1.5)


def list_profiles():
    return sorted(f[:-5] for f in os.listdir(PROFILES_DIR) if f.endswith(".conf")) \
        if os.path.isdir(PROFILES_DIR) else []


def installed_profiles():
    if not os.path.isdir(CONF_D):
        return []
    return sorted(f[len(DEPLOY_PREFIX):-5] for f in os.listdir(CONF_D)
                  if f.startswith(DEPLOY_PREFIX) and f.endswith(".conf"))


def pick(prompt, choices, default=None):
    """Numbered picker (menu to stderr). Enter accepts default; a name is echoed as-is."""
    print("Available:", file=sys.stderr)
    for i, c in enumerate(choices, 1):
        print(f"  {i}) {c}", file=sys.stderr)
    label = f"{prompt} [{default}]: " if default else f"{prompt}: "
    try:
        ans = input(label).strip()
    except EOFError:
        ans = ""
    if not ans and default:
        return default
    if ans.isdigit() and 1 <= int(ans) <= len(choices):
        return choices[int(ans) - 1]
    return ans or None


# ─────────────────────────── subcommands ────────────────────────────────────

def cmd_measure(args):
    speaker, mic, _ = resolve_devices(args.speaker, args.mic)
    print("\nKeep the room quiet. Playing sweeps...\n")
    with tempfile.TemporaryDirectory() as tmp:
        sweep = os.path.join(tmp, "sweep.wav")
        n_settle, n_sweep = generate_sweep(sweep)
        measurements, freqs = [], None
        for i in range(1, args.iterations + 1):
            print(f"--- pass {i}/{args.iterations} ---")
            rec = os.path.join(tmp, f"rec{i}.wav")
            play_and_record(sweep, rec, speaker, mic)
            freqs, db = analyze_response(sweep, rec, n_settle, n_sweep)
            if freqs is None:
                continue
            measurements.append(db)
            print_response(freqs, db, f"pass {i} (relative)")
        if len(measurements) > 1:
            np = _np()
            print_response(freqs, np.mean(measurements, axis=0), f"AVERAGE of {len(measurements)} passes")
    print("\nMeasurement complete — nothing changed.")


def cmd_create(args):
    name = args.name
    speaker, mic, nick = resolve_devices(args.speaker, args.mic)
    target_match = args.target_match or nick or speaker
    info(f"target-match for '{name}': {target_match}")
    print("\nKeep the room quiet. Measuring to design a starting EQ...\n")
    with tempfile.TemporaryDirectory() as tmp:
        sweep = os.path.join(tmp, "sweep.wav")
        n_settle, n_sweep = generate_sweep(sweep)
        measurements, freqs = [], None
        for i in range(1, args.iterations + 1):
            print(f"--- pass {i}/{args.iterations} ---")
            rec = os.path.join(tmp, f"rec{i}.wav")
            play_and_record(sweep, rec, speaker, mic)
            freqs, db = analyze_response(sweep, rec, n_settle, n_sweep)
            if freqs is not None:
                measurements.append(db)
        if not measurements:
            err("No usable measurements."); sys.exit(1)
        np = _np()
        avg = np.mean(measurements, axis=0)
        print_response(freqs, avg, "measured (average)")
        bands = design_eq(freqs, avg)
    out = os.path.join(PROFILES_DIR, f"{name}.conf")
    write_profile(name, bands, args.boost, target_match, out)
    ok(f"Wrote {out}  ({len(bands)} bands, +{args.boost} dB makeup)")
    print("  This is a STARTING point. Edit it, then `install` and listen; re-measure to refine.")


def cmd_list(_args):
    profs, inst = list_profiles(), installed_profiles()
    default = subprocess.run(["pactl", "get-default-sink"], capture_output=True, text=True).stdout.strip()
    if not profs:
        warn(f"No profiles in {PROFILES_DIR}"); return
    print(f"\nProfiles in {PROFILES_DIR}:\n")
    for p in profs:
        node = profile_input_node(os.path.join(PROFILES_DIR, f"{p}.conf")) or ""
        flags = []
        if p in inst:
            flags.append("installed")
        if node and node == default:
            flags.append("ACTIVE default")
        tm = profile_field(os.path.join(PROFILES_DIR, f"{p}.conf"), "target-match") or "?"
        state = f"  [{', '.join(flags)}]" if flags else ""
        print(f"  • {p:<22} → {tm}{state}")
    print()


def cmd_install(args):
    profs = list_profiles()
    if not profs:
        err(f"No profiles in {PROFILES_DIR}"); sys.exit(1)
    name = args.name or pick("Pick a profile to install", profs, default=profs[0])
    if not name:
        warn("Nothing selected."); return
    path = os.path.join(PROFILES_DIR, f"{name}.conf")
    if not os.path.isfile(path):
        err(f"Unknown profile '{name}'. Available: {', '.join(profs)}"); sys.exit(1)

    match = profile_field(path, "target-match")
    if not match:
        err(f"Profile '{name}' has no '# target-match:' line."); sys.exit(1)
    hits = resolve_sink(args.speaker) if args.speaker else resolve_sink(match)
    if not hits:
        err(f"No sink matches '{args.speaker or match}' — is that speaker/monitor connected?")
        print("  Connected sinks:")
        for d in get_pactl_devices("sinks"):
            print(f"    {d['name']}")
        sys.exit(1)
    target = hits[0] if len(hits) == 1 else pick(f"Target sink for '{name}'", hits, default=hits[0])
    if not target:
        warn("No target chosen."); return

    with open(path) as f:
        text = f.read().replace("__TARGET_SINK__", target)
    os.makedirs(CONF_D, exist_ok=True)
    dest = os.path.join(CONF_D, f"{DEPLOY_PREFIX}{name}.conf")
    with open(dest, "w") as f:
        f.write(text)
    ok(f"Installed '{name}' → {target}")
    info("Restarting PipeWire...")
    restart_pipewire()
    node = profile_input_node(path)
    if node and subprocess.run(["pactl", "set-default-sink", node], capture_output=True).returncode == 0:
        ok(f"Default output set to {node}")
    else:
        warn("Loaded, but couldn't set default — pick it in your sound settings.")


def cmd_uninstall(args):
    inst = installed_profiles()
    if not inst:
        warn("No installed lab profiles."); return
    name = args.name or pick("Pick a profile to uninstall", inst, default=inst[0])
    if not name:
        warn("Nothing selected."); return
    dest = os.path.join(CONF_D, f"{DEPLOY_PREFIX}{name}.conf")
    if not os.path.isfile(dest):
        err(f"'{name}' is not installed."); sys.exit(1)
    os.remove(dest)
    ok(f"Removed {dest}")
    info("Restarting PipeWire...")
    restart_pipewire()


def cmd_promote(args):
    path = os.path.join(PROFILES_DIR, f"{args.name}.conf")
    if not os.path.isfile(path):
        err(f"Unknown profile '{args.name}'."); sys.exit(1)
    if not os.path.isdir(SIBLING_RIS_ASSETS):
        err(f"ReinstallScripts assets not found at {SIBLING_RIS_ASSETS}")
        print("  Expected a sibling checkout: ../ReinstallScripts")
        sys.exit(1)
    dest = os.path.join(SIBLING_RIS_ASSETS, f"{args.name}.conf")
    with open(path) as s, open(dest, "w") as d:
        d.write(s.read())
    ok(f"Promoted → {dest}")
    print("  Commit it in ReinstallScripts and install there with `just speaker-eq`.")


def main():
    p = argparse.ArgumentParser(description="Measure, build, and install PipeWire speaker EQ profiles.")
    sub = p.add_subparsers(dest="cmd", required=True)

    def add_measure_opts(sp):
        sp.add_argument("--speaker", help="speaker sink name (default: auto-detect / prompt)")
        sp.add_argument("--mic", help="mic source name (default: auto-detect / prompt)")
        sp.add_argument("--iterations", type=int, default=DEFAULT_ITERATIONS)

    m = sub.add_parser("measure", help="measure and print the response; no changes")
    add_measure_opts(m); m.set_defaults(func=cmd_measure)

    c = sub.add_parser("create", help="measure + generate a starting profile")
    c.add_argument("name")
    c.add_argument("--boost", type=float, default=0.0, help="makeup gain in dB (default 0 = none)")
    c.add_argument("--target-match", help="sink match string (default: the speaker's EDID/nick name)")
    add_measure_opts(c); c.set_defaults(func=cmd_create)

    ls = sub.add_parser("list", help="list profiles and their install state")
    ls.set_defaults(func=cmd_list)

    ins = sub.add_parser("install", help="install a profile as an EQ output")
    ins.add_argument("name", nargs="?")
    ins.add_argument("--speaker", help="override the target sink match")
    ins.set_defaults(func=cmd_install)

    un = sub.add_parser("uninstall", help="remove an installed profile")
    un.add_argument("name", nargs="?")
    un.set_defaults(func=cmd_uninstall)

    pr = sub.add_parser("promote", help="copy a profile into a sibling ReinstallScripts checkout")
    pr.add_argument("name")
    pr.set_defaults(func=cmd_promote)

    args = p.parse_args()
    args.func(args)


if __name__ == "__main__":
    main()
