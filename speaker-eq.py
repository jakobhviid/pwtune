#!/usr/bin/env python3
"""
pipewire-speaker-calibration — measure any speaker with any mic, build a PipeWire
filter-chain EQ profile, and install/remove profiles.

This is the tool. The justfile is only a thin set of shortcuts over these
subcommands, so everything works with plain `python3 speaker-eq.py <cmd>` too.

Profiles live in two folders: drafts/ (scratch, where `create` writes) and
calibrated/ (finalized, shipped). `promote` moves a draft into calibrated/;
calibrated profiles are final — `edit` and `delete` refuse to touch them.

Subcommands:
  measure               Play a sweep, record it, print the frequency response. No changes.
  create   [name]       measure + auto-generate a *starting* EQ profile -> drafts/<name>.conf
  list                  List profiles (draft + shipped) and their install state
  install  [name]       Resolve the profile's target sink, deploy it, make it the default output
  edit     [name]       Open a draft profile in $EDITOR (calibrated ones are final)
  delete   [name]       Delete a draft profile (calibrated ones are final)
  uninstall [name]      Remove an installed profile
  promote  [name]       Finalize a draft: move drafts/<name>.conf -> calibrated/<name>.conf

Measurement honesty: auto-generated EQ is a STARTING POINT, not a finished tuning.
Cheap mics (webcams, headset mics) have a noise floor, comb filtering off nearby
surfaces, and mains hum — trust the low/low-mid bands, use your ears up top, and
iterate measure -> edit profiles/<name>.conf -> re-measure.
"""

import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

REPO_DIR = os.path.dirname(os.path.abspath(__file__))
DRAFTS_DIR = os.path.join(REPO_DIR, "drafts")          # scratch; `create` writes here (gitignored)
CALIBRATED_DIR = os.path.join(REPO_DIR, "calibrated")  # finalized, shipped profiles (committed)
CONF_D = os.path.expanduser("~/.config/pipewire/pipewire.conf.d")
DEPLOY_PREFIX = "eqlab-"           # so lab installs never clash with other tools' files

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

# How the user invoked us, so "next step" hints are copy-pasteable. The justfile
# exports SPEAKER_EQ_LAUNCHER=just; running the script directly falls back to it.
LAUNCHER = os.environ.get("SPEAKER_EQ_LAUNCHER", "python3 speaker-eq.py")

def run_cmd(verb, arg=""):
    return f"{LAUNCHER} {verb}" + (f" {arg}" if arg else "")

def next_steps(*items):
    """Print a short 'what to do next' block. Each item is (description, command|'')."""
    print(f"\n{C['blue']}  Next:{C['off']}")
    for desc, cmd in items:
        print(f"    • {desc}:  {cmd}" if cmd else f"    • {desc}")


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


def ask(prompt, default=""):
    """Free-text prompt with an optional default (Enter accepts it)."""
    label = f"{prompt} [{default}]: " if default != "" else f"{prompt}: "
    try:
        ans = input(label).strip()
    except EOFError:
        ans = ""
    return ans or default


def choose_device(kind, devices, detected):
    """Numbered picker over devices; the detected one is the Enter-default."""
    if not devices:
        err(f"No {kind}s found. Is PipeWire running?"); sys.exit(1)
    print(f"\nAvailable {kind}s:", file=sys.stderr)
    default_n = None
    for i, d in enumerate(devices, 1):
        label = d.get("nick") or d.get("desc") or d["name"]
        is_def = detected and d["name"] == detected["name"]
        if is_def:
            default_n = i
        print(f"  {i}) {label}{'   ← detected (press Enter)' if is_def else ''}", file=sys.stderr)
        print(f"       {d['name']}", file=sys.stderr)
    while True:
        prompt = f"Pick a {kind}" + (f" [{default_n}]: " if default_n else ": ")
        try:
            ans = input(prompt).strip()
        except EOFError:
            ans = ""
        if not ans and default_n:
            return devices[default_n - 1]
        if ans.isdigit() and 1 <= int(ans) <= len(devices):
            return devices[int(ans) - 1]
        print("  Please enter a number from the list.", file=sys.stderr)


def resolve_devices(speaker_arg, mic_arg):
    """Resolve (speaker_sink, mic_source, speaker_nick). Always guides the user
    through a numbered picker (defaulting to the auto-detected device) unless a
    device was given explicitly on the command line."""
    if speaker_arg:
        speaker = {"name": speaker_arg, "desc": speaker_arg, "nick": ""}
    else:
        sinks = [d for d in get_pactl_devices("sinks")
                 if not d["name"].startswith(("effect_input.", "effect_output."))]
        speaker = choose_device("speaker", sinks, detect_speaker_sink(sinks))

    if mic_arg:
        mic = {"name": mic_arg, "desc": mic_arg, "nick": ""}
    else:
        sources = [d for d in get_pactl_devices("sources") if "monitor" not in d["name"].lower()]
        mic = choose_device("microphone", sources, detect_mic_source(sources))

    info(f"Speaker: {speaker.get('nick') or speaker.get('desc') or speaker['name']}")
    info(f"Mic:     {mic.get('nick') or mic.get('desc') or mic['name']}")
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
    try:
        import numpy as np
        from scipy import signal  # noqa: F401  (availability check)
        return np
    except ImportError:
        err("NumPy + SciPy are required for measurement but aren't installed.")
        print("  Install them:   python3 -m pip install numpy scipy")
        print("  Or use the justfile — `just measure` / `just create` set up a local")
        print("  environment automatically on first run, so you don't have to.")
        sys.exit(1)


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


def profile_description(path):
    """The human-facing name (node.description) shown in sound settings."""
    with open(path) as f:
        m = re.search(r'node\.description\s*=\s*"([^"]+)"', f.read())
    return m.group(1) if m else "(no description)"


# ─────────────────────────── deploy / manage ────────────────────────────────

def restart_pipewire():
    subprocess.run(["systemctl", "--user", "restart", "pipewire", "pipewire-pulse"], capture_output=True)
    time.sleep(1.5)


def _names_in(d):
    return sorted(f[:-5] for f in os.listdir(d) if f.endswith(".conf")) if os.path.isdir(d) else []


def draft_names():
    return _names_in(DRAFTS_DIR)


def profile_names():
    """All profile names, drafts first (active work), de-duplicated."""
    seen, out = set(), []
    for n in draft_names() + _names_in(CALIBRATED_DIR):
        if n not in seen:
            seen.add(n); out.append(n)
    return out


def find_profile(name):
    """(path, tier) for a profile name, preferring a draft over a shipped one; else (None, None)."""
    for tier, d in (("draft", DRAFTS_DIR), ("shipped", CALIBRATED_DIR)):
        path = os.path.join(d, f"{name}.conf")
        if os.path.isfile(path):
            return path, tier
    return None, None


def installed_profiles():
    if not os.path.isdir(CONF_D):
        return []
    return sorted(f[len(DEPLOY_PREFIX):-5] for f in os.listdir(CONF_D)
                  if f.startswith(DEPLOY_PREFIX) and f.endswith(".conf"))


def deployed_eq_configs():
    """Every speaker-EQ filter-chain config currently in the PipeWire config dir,
    so uninstall can clean up anything lingering — not just this tool's own installs.
    Returns [(label, path, kind)] with kind in draft|shipped|orphan|external:
      - our installs (eqlab-<name>.conf): kind = the source profile's tier, or
        'orphan' if that profile is no longer in the repo.
      - any OTHER .conf that is a filter-chain EQ (e.g. an old speaker-eq.conf /
        monitor-eq.conf, or one deployed by another tool): kind = 'external'."""
    out = []
    if not os.path.isdir(CONF_D):
        return out
    for f in sorted(os.listdir(CONF_D)):
        if not f.endswith(".conf"):
            continue
        path = os.path.join(CONF_D, f)
        if f.startswith(DEPLOY_PREFIX):
            name = f[len(DEPLOY_PREFIX):-5]
            _, tier = find_profile(name)
            out.append((name, path, tier or "orphan"))
        else:
            try:
                txt = open(path).read()
            except OSError:
                continue
            if "libpipewire-module-filter-chain" in txt and "effect_input" in txt:
                out.append((f, path, "external"))   # label = filename (may collide with a bare name)
    return out


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


def pick_sections(prompt, sections, default=None):
    """Picker split into labelled sections, e.g. drafts on top, calibrated below.
    `sections` is a list of (header, [names]); numbering is continuous across them
    so a single number selects. Enter accepts `default`; a typed name is echoed."""
    flat = []
    print(file=sys.stderr)
    idx = 1
    for header, names in sections:
        if not names:
            continue
        print(f"  {header}:", file=sys.stderr)
        for n in names:
            mark = "   ← default (Enter)" if default and n == default else ""
            print(f"    {idx}) {n}{mark}", file=sys.stderr)
            flat.append(n)
            idx += 1
    if not flat:
        return None
    label = f"{prompt} [{default}]: " if default else f"{prompt}: "
    try:
        ans = input(label).strip()
    except EOFError:
        ans = ""
    if not ans and default:
        return default
    if ans.isdigit() and 1 <= int(ans) <= len(flat):
        return flat[int(ans) - 1]
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
    next_steps(
        ("Build an EQ from a speaker like this", run_cmd("create")),
        ("Install a profile you already have", run_cmd("install")),
    )


def cmd_create(args):
    speaker, mic, nick = resolve_devices(args.speaker, args.mic)
    name = args.name or ask("\nName this profile", slugify(nick) if nick else "my-speaker")
    if not name:
        warn("No name given — aborting."); return
    boost = args.boost
    if boost is None:
        try:
            boost = float(ask("Makeup gain in dB (louder overall; 0 = none)", "0"))
        except ValueError:
            boost = 0.0
    target_match = args.target_match or nick or speaker
    info(f"Profile '{name}': target-match='{target_match}', makeup +{boost} dB")
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
    out = os.path.join(DRAFTS_DIR, f"{name}.conf")
    write_profile(name, bands, boost, target_match, out)
    ok(f"Wrote draft {out}  ({len(bands)} bands, +{boost} dB makeup)")
    print("  A starting point: the low/low-mid correction is measured; the top end")
    print("  is a guess (the mic barely hears it) — refine it by ear.")
    next_steps(
        ("Hear it", run_cmd("install", name)),
        ("Tweak it", f"{run_cmd('edit', name)}, then re-run {run_cmd('install', name)}"),
        ("Finalize it (ship it)", run_cmd("promote", name)),
    )


def cmd_list(_args):
    names = profile_names()
    inst = installed_profiles()
    default = subprocess.run(["pactl", "get-default-sink"], capture_output=True, text=True).stdout.strip()
    if not names:
        warn("No profiles yet — make one with `create`."); return
    print()
    for p in names:
        path, tier = find_profile(p)
        desc = profile_description(path)
        tm = profile_field(path, "target-match") or ""
        connected = bool(resolve_sink(tm)) if tm else False
        node = profile_input_node(path)
        tags = ["shipped" if tier == "shipped" else "draft"]
        if p in inst:
            tags.append("installed")
        if node and node == default:
            tags.append("active output")
        dot = "●" if connected else "○"
        print(f"  {dot} {p:<22}{desc:<30}[{' · '.join(tags)}]")
    print("\n  ● speaker connected now    ○ not connected")
    print("  draft = editable work in progress   ·   shipped = finalized (frozen)")
    next_steps(
        ("Install one", run_cmd("install") + "  (pick from the list)"),
        ("Create a new one", run_cmd("create")),
    )


def cmd_install(args):
    profs = profile_names()
    if not profs:
        err("No profiles yet — make one with `create`."); sys.exit(1)
    # Machine-aware default: the profile whose target speaker is connected now.
    default = None
    for p in profs:
        path, _ = find_profile(p)
        m = profile_field(path, "target-match")
        if m and resolve_sink(m):
            info(f"Connected speaker detected → default profile '{p}'")
            default = p
            break
    sections = [("Drafts (work in progress)", draft_names()),
                ("Calibrated (promoted / shipped)", _names_in(CALIBRATED_DIR))]
    name = args.name or pick_sections("Pick a profile to install", sections, default=default or profs[0])
    if not name:
        warn("Nothing selected."); return
    path, tier = find_profile(name)
    if not path:
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
    print(f"\n  '{name}' is now your output — play something to hear it.")
    steps = []
    if tier == "draft":
        steps.append(("Re-tune", f"{run_cmd('edit', name)}, then re-run {run_cmd('install', name)}"))
    steps.append(("Turn it off", run_cmd("uninstall", name)))
    if tier == "draft":
        steps.append(("Finalize it (ship it)", run_cmd("promote", name)))
    next_steps(*steps)


def cmd_uninstall(args):
    items = deployed_eq_configs()
    if not items:
        warn(f"No EQ configs are deployed in {CONF_D}."); return
    buckets = {"draft": [], "shipped": [], "orphan": [], "external": []}
    label_path = {}
    for label, path, kind in items:
        buckets[kind].append(label)
        label_path[label] = path
    sections = [
        ("Drafts (work in progress)", buckets["draft"]),
        ("Calibrated (promoted / shipped)", buckets["shipped"]),
        ("Orphans (installed, but the profile is gone from the repo)", buckets["orphan"]),
        ("Other EQ configs on this system (not from this tool)", buckets["external"]),
    ]
    order = buckets["draft"] + buckets["shipped"] + buckets["orphan"] + buckets["external"]
    name = args.name or pick_sections("Pick an EQ to uninstall", sections, default=order[0])
    if not name:
        warn("Nothing selected."); return
    # A CLI-given name is the profile name (eqlab-<name>.conf); a picked label may be
    # a raw filename for an external config.
    path = label_path.get(name) or os.path.join(CONF_D, f"{DEPLOY_PREFIX}{name}.conf")
    if not os.path.isfile(path):
        err(f"'{name}' is not installed."); return
    os.remove(path)
    ok(f"Removed {path}")
    info("Restarting PipeWire...")
    restart_pipewire()
    print("\n  EQ removed — audio goes straight to the speaker again.")
    next_steps(
        ("See all profiles", run_cmd("list")),
        ("Install one", run_cmd("install")),
    )


def cmd_edit(args):
    drafts = draft_names()
    if not drafts:
        err("No draft profiles to edit — make one with `create`."); return
    name = args.name or pick("Pick a draft to edit", drafts, default=drafts[0])
    if not name:
        warn("Nothing selected."); return
    path, tier = find_profile(name)
    if not path:
        err(f"Unknown profile '{name}'. Drafts: {', '.join(drafts)}"); return
    if tier == "shipped":
        err(f"'{name}' is finalized (in calibrated/) and frozen — edit is for drafts only.")
        print("  To change it, create a new draft (re-measure) and promote again.")
        return
    editor = os.environ.get("EDITOR") or shutil.which("nano") or shutil.which("vi") or "vi"
    info(f"Opening {path} in {editor} …")
    subprocess.run([editor, path])
    next_steps(("Try your changes", run_cmd("install", name)))


def cmd_delete(args):
    drafts = draft_names()
    if not drafts:
        err("No draft profiles to delete."); return
    name = args.name or pick("Pick a draft to delete", drafts, default=drafts[0])
    if not name:
        warn("Nothing selected."); return
    path, tier = find_profile(name)
    if not path:
        err(f"Unknown profile '{name}'. Drafts: {', '.join(drafts)}"); return
    if tier == "shipped":
        err(f"'{name}' is finalized (in calibrated/) and frozen — delete is for drafts only.")
        return
    if name in installed_profiles():
        warn(f"'{name}' is currently installed — uninstall it first if you don't want it playing.")
    if not ask(f"Delete drafts/{name}.conf?", "n").lower().startswith("y"):
        warn("Kept."); return
    os.remove(path)
    ok(f"Deleted drafts/{name}.conf")


def cmd_promote(args):
    drafts = draft_names()
    if not drafts:
        err("No drafts to finalize — make one with `create`."); return
    name = args.name or pick("Pick a draft to finalize", drafts, default=drafts[0])
    if not name:
        warn("Nothing selected."); return
    src = os.path.join(DRAFTS_DIR, f"{name}.conf")
    if not os.path.isfile(src):
        _, tier = find_profile(name)
        if tier == "shipped":
            warn(f"'{name}' is already finalized (in calibrated/)."); return
        err(f"No draft named '{name}'. Drafts: {', '.join(drafts)}"); return
    os.makedirs(CALIBRATED_DIR, exist_ok=True)
    dest = os.path.join(CALIBRATED_DIR, f"{name}.conf")
    if os.path.exists(dest) and not ask(f"calibrated/{name}.conf exists — overwrite?", "n").lower().startswith("y"):
        warn("Kept the existing one; draft left in place."); return
    os.replace(src, dest)   # move: a finalized profile lives in exactly one place
    ok(f"Finalized: drafts/{name}.conf → calibrated/{name}.conf")
    print("  It's frozen now (edit/delete won't touch it) and ships with the repo.")
    next_steps(
        ("Publish it", "git add calibrated/ && commit && push"),
        ("Deploy on a machine", "in ReinstallScripts: just eq-import, then just speaker-eq"),
    )


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
    c.add_argument("name", nargs="?", help="profile name (prompted if omitted)")
    c.add_argument("--boost", type=float, default=None, help="makeup gain in dB (prompted if omitted; 0 = none)")
    c.add_argument("--target-match", help="sink match string (default: the speaker's EDID/nick name)")
    add_measure_opts(c); c.set_defaults(func=cmd_create)

    ls = sub.add_parser("list", help="list profiles and their install state")
    ls.set_defaults(func=cmd_list)

    ins = sub.add_parser("install", help="install a profile as an EQ output")
    ins.add_argument("name", nargs="?")
    ins.add_argument("--speaker", help="override the target sink match")
    ins.set_defaults(func=cmd_install)

    ed = sub.add_parser("edit", help="open a draft profile in $EDITOR (calibrated ones are frozen)")
    ed.add_argument("name", nargs="?")
    ed.set_defaults(func=cmd_edit)

    dl = sub.add_parser("delete", help="delete a draft profile (calibrated ones are frozen)")
    dl.add_argument("name", nargs="?")
    dl.set_defaults(func=cmd_delete)

    un = sub.add_parser("uninstall", help="remove an installed profile")
    un.add_argument("name", nargs="?")
    un.set_defaults(func=cmd_uninstall)

    pr = sub.add_parser("promote", help="finalize a draft: move it into calibrated/ (shipped, frozen)")
    pr.add_argument("name", nargs="?", help="profile name (prompted if omitted)")
    pr.set_defaults(func=cmd_promote)

    args = p.parse_args()
    try:
        args.func(args)
    except KeyboardInterrupt:
        print("\nCancelled.", file=sys.stderr)
        sys.exit(0)


if __name__ == "__main__":
    main()
