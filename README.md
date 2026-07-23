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

Works on any desktop that uses PipeWire (GNOME, KDE, Sway, …) — it talks to the
audio server, not the desktop.

## Dependencies

| Need | When | Notes |
|------|------|-------|
| **PipeWire** (`paplay`, `parecord`, `pactl`) | **runtime** | The only runtime requirement. Ships with `pipewire`/`pipewire-pulse`, default on most modern distros. Check: `pactl info \| grep "Server Name"` |
| **Rust** (`cargo`) | **build only** | To compile the binary once. Install via [rustup](https://rustup.rs) or your package manager (`brew install rust`, `dnf install cargo`, …). |
| `just` | optional | Only for the `just` shortcuts; the binary works without it. |

There is **no runtime dependency on Python, NumPy, or SciPy** — the analysis
(sweep synthesis, Welch PSD via FFT, EQ design) is built into the binary. Once
compiled it's a single self-contained executable.

## Build

```bash
cargo build --release        # or: just build
# → ./target/release/speaker-eq
```

## Usage

The tool is the `speaker-eq` binary:

```bash
./target/release/speaker-eq <command> [options]
# (put it on your PATH, or use the `just` shortcuts below)
```

Profiles live in two folders: **`drafts/`** (scratch, where `create` writes —
gitignored) and **`calibrated/`** (finalized, shipped with the repo). `promote`
moves a draft into `calibrated/`; calibrated profiles are **final** — `edit` and
`delete` refuse to touch them.

| Command | What it does |
|---------|--------------|
| `measure` | Play a sweep, record it, print the frequency response. Changes nothing. |
| `create [name]` | Measure, then auto-generate a starting profile → `drafts/<name>.conf`. Prompts for name/boost if omitted. |
| `list` | List profiles (draft + shipped) and show which are installed / the active output. |
| `install [name]` | Resolve the profile's target sink, deploy it, make it the default output. Prompts if no name. |
| `edit [name]` | Open a **draft** profile in `$EDITOR`. Prompts if no name. |
| `delete [name]` | Delete a **draft** profile (asks to confirm). Prompts if no name. |
| `uninstall [name]` | Remove a deployed EQ. The picker is sectioned — drafts / calibrated / **orphaned** (installed but the profile left the repo) / **other** EQ configs found on the system — so you can clean up leftovers too, not just current installs. |
| `promote [name]` | Finalize a draft: **move** it into `calibrated/` (shipped, frozen). Prompts if no name. |

Common options for `measure`/`create`: `--speaker <sink>`, `--mic <source>`
(both auto-detect or prompt if omitted), `--iterations N`. `create` also takes
`--boost <dB>` (default `0` = none) and `--target-match <string>`.

Find device names with `pactl list short sinks` and `pactl list short sources`.

### `just` shortcuts (optional)

If you have [`just`](https://github.com/casey/just), the `justfile` mirrors the
CLI so you don't have to remember the exact invocation — it's a convenience
wrapper only:

```bash
just measure                     # → ./target/release/speaker-eq measure
just create                      # guided: pick speaker/mic, name it, set boost
just list
just install                     # defaults to the connected speaker
just edit desk-monitor           # tweak a draft
just promote desk-monitor        # finalize it into calibrated/
```

Every verb runs with no arguments too — it enumerates what's available, defaults
to the sensible choice for this machine, and prompts for the rest.

## A typical session

```bash
# 1. See what your speaker actually does
just measure

# 2. Generate a draft profile (it prompts for a name, boost, speaker, mic)
just create

# 3. Install it and listen
just install desk-monitor

# 4. Not right? Tweak the draft (Gain/Freq/Q are plain text), re-install, re-check
just edit desk-monitor
just install desk-monitor

# 5. Happy with it? Finalize it — moves drafts/ → calibrated/ (shipped, frozen)
just promote desk-monitor
git add calibrated/ && git commit -m "add desk-monitor profile" && git push
```

## Drafts vs. calibrated

- **`drafts/`** — where `create` writes and you iterate. Gitignored: your
  experiments stay local and don't ship. `edit`/`delete` work here.
- **`calibrated/`** — the finalized profiles the repo ships. `promote` moves a
  draft here; once finalized it's **frozen** (`edit`/`delete` refuse it). To
  change a shipped profile, make a fresh draft and promote again.

## Using these profiles elsewhere (e.g. ReinstallScripts)

This repo depends on nothing and knows nothing about its consumers. If you
provision machines with a dotfiles/setup repo, have **that** repo pull the
`calibrated/*.conf` from here (cloning this repo if needed) — the dependency
belongs on the consumer's side. (In the author's
[ReinstallScripts](https://github.com/jakobhviid/ReinstallScripts), `just
eq-import` does exactly this.)

## How profiles work

Each profile is a PipeWire `filter-chain` config (`drafts/<name>.conf` while
you're working on it, `calibrated/<name>.conf` once finalized). It creates a
**virtual sink** you select as your output; audio routes through the EQ before
reaching the real speaker.

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
which fixed one laptop's speakers. It's now a general tool for any speaker,
rewritten from the original Python prototype into a single self-contained Rust
binary (no NumPy/SciPy runtime). Two profiles ship in `calibrated/` as examples:
`thinkpad-x1carbon.conf` (that laptop's internal speakers) and
`dell-u4025qw.conf` (a monitor's built-in speakers over HDMI/DisplayPort).

## License

MIT
