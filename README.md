# pipewire-speaker-calibration

Measure any speaker with any microphone, build a [PipeWire](https://pipewire.org/)
filter-chain EQ profile from what you hear, and install/remove profiles on the fly.

It plays a logarithmic sine sweep out a speaker, records it back through a mic,
computes the frequency response, and can auto-generate a parametric EQ that
corrects toward a target curve. Profiles are plain PipeWire config files you can
hand-edit, keep, install, and remove — so you can experiment with as many tunings
as you like and A/B them by ear.

> **This is a calibration *lab*, not a magic button.** Auto-generated EQ is a
> *starting point*. Cheap mics (webcams, headset mics) have a noise floor, comb
> filtering off nearby surfaces, and mains hum — they lie, especially up top.
> Trust the low / low-mid bands, use your ears above ~1–2 kHz, and iterate:
> `measure` → edit the profile → `install` → listen → re-measure.

## Dependencies

| Need | Package | Notes |
|------|---------|-------|
| **PipeWire** as the audio server | `pipewire`, `wireplumber` | Default on most modern distros. Check: `pactl info \| grep "Server Name"` |
| `paplay`, `parecord`, `pactl` | ships with `pipewire` / `pipewire-pulse` | Playback, recording, and device queries |
| `wpctl` | `wireplumber` | Optional; not required |
| **Python 3** + **NumPy** + **SciPy** | see below | Sweep generation and frequency analysis |
| `just` | `just` | **Optional** — only for the `just` shortcuts. The Python CLI works without it. |

Install the Python libraries:

```bash
# Fedora
sudo dnf install -y python3-numpy python3-scipy

# Ubuntu/Debian
sudo apt install -y python3-numpy python3-scipy

# Arch
sudo pacman -S python-numpy python-scipy

# or, isolated in a venv (any distro)
python3 -m venv .venv && . .venv/bin/activate && pip install numpy scipy
```

`paplay`/`parecord`/`pactl` come with PipeWire on essentially every distro that
uses it, so you usually don't need to install anything else.

## Usage

The tool is the Python CLI — it's self-contained (no `just` required):

```bash
python3 speaker-eq.py <command> [options]
```

| Command | What it does |
|---------|--------------|
| `measure` | Play a sweep, record it, print the frequency response. Changes nothing. |
| `create <name>` | Measure, then auto-generate a starting profile → `profiles/<name>.conf`. |
| `list` | List profiles and show which are installed / which is the active default output. |
| `install [name]` | Resolve the profile's target sink, deploy it, make it the default output. Prompts if no name. |
| `uninstall [name]` | Remove an installed profile. Prompts if no name. |
| `promote <name>` | Copy a finished profile into a sibling `ReinstallScripts` checkout (personal helper). |

Common options for `measure`/`create`: `--speaker <sink>`, `--mic <source>`
(both auto-detect or prompt if omitted), `--iterations N`. `create` also takes
`--boost <dB>` (default `0` = none) and `--target-match <string>`.

Find device names with `pactl list short sinks` and `pactl list short sources`.

### `just` shortcuts (optional)

If you have [`just`](https://github.com/casey/just), the `justfile` mirrors the
CLI so you don't have to remember the exact invocation — it's a convenience
wrapper only:

```bash
just measure                     # → python3 speaker-eq.py measure
just create desk-monitor --boost 12
just list
just install desk-monitor
just uninstall desk-monitor
```

## A typical session

```bash
# 1. See what your speaker actually does
python3 speaker-eq.py measure --iterations 5

# 2. Generate a starting profile (name it, add makeup gain if the speaker is quiet)
python3 speaker-eq.py create desk-monitor --boost 12

# 3. Install it and listen
python3 speaker-eq.py install desk-monitor

# 4. Not right? Edit profiles/desk-monitor.conf (Gain/Freq/Q values are plain text),
#    re-install, and re-measure to check.
python3 speaker-eq.py install desk-monitor
python3 speaker-eq.py measure          # compare against the original
```

## How profiles work

Each profile is a PipeWire `filter-chain` config in `profiles/<name>.conf`. It
creates a **virtual sink** you select as your output; audio routes through the
EQ before reaching the real speaker.

- **Portable targets.** Instead of hardcoding a sink, each profile carries a
  `# target-match:` line — a string matched against the sinks present at install
  time (`node.name`, and the monitor's EDID name via `node.nick`). Display-audio
  sink names differ per machine/GPU/bus, but a monitor's EDID name is the same
  wherever it's plugged in, so one profile follows the device across machines.
- **Coexist.** Each profile gets its own node name, so several can be installed at
  once — e.g. a laptop EQ'ing both its internal speakers and an external monitor;
  you switch between them in your sound settings.
- **Makeup gain is broadband and digital.** `--boost` adds shelf stages that lift
  the whole spectrum. It has a hard ceiling at 0 dBFS — past that it clips rather
  than getting louder, so for clean loudness use the speaker's/monitor's own
  volume. Raise or lower the `preamp` `Gain` values in the profile to taste.

## Notes / limitations

- Readings above roughly 1.5–2 kHz depend heavily on the mic and on reflections;
  treat the high end as your ears' domain, not the analyzer's.
- Keep the room quiet and the mic close to (and pointed at) the speaker.
- The measurement passes through whatever EQ is currently active, so you can
  measure "before" (no profile installed) vs "after" to see the effect.

## History

This grew out of
[thinkpad-x1-carbon-pipewire-eq](https://github.com/jakobhviid/thinkpad-x1-carbon-pipewire-eq),
which fixed one laptop's speakers. It's now a general tool for any speaker; the
bundled `profiles/thinkpad-x1carbon.conf` remains as an example.

## License

MIT
