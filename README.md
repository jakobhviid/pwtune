# pwtune

Measure any speaker with any microphone, build a [PipeWire](https://pipewire.org/)
filter-chain EQ profile from what you hear, and install/remove profiles — a small,
**folder-scoped** command-line tool.

It plays a logarithmic sine sweep out a speaker, records it back through a mic,
computes the frequency response, and auto-generates a parametric EQ that corrects
toward a target curve. Profiles are plain PipeWire config files you edit, keep,
install, and remove.

> **A calibration *aid*, not a magic button.** Auto-generated EQ is a *starting
> point*. Cheap mics (webcams, headset mics) have a noise floor, comb filtering,
> and mains hum — they lie, especially up top. Trust the low / low-mid bands, use
> your ears above ~1–2 kHz, and iterate: `measure` → edit → `install` → listen →
> re-measure.

## How it works: it operates on the current folder

`pwtune` works on the `.conf` files in **whatever directory you run it in** —
like a linter or formatter. There's no hidden library or home directory. `cd` to
wherever you want your profiles to live (a scratch dir, a dotfiles repo, …), and
the tool acts on that folder.

A profile is "calibrated" (frozen) purely by its **filename**:

- `desk.conf` — a **draft**: editable, deletable, tweak away.
- `desk.calibrated.conf` — **calibrated**: frozen. `edit`/`delete` refuse it.

`promote` is just a rename (`desk.conf` → `desk.calibrated.conf`) — its only job
is that lock. You commit the files yourself, wherever you keep them.

## Install

```sh
brew install jakobhviid/tap/pwtune      # Linux (x86_64 / arm64)
```

Or build from source (needs Rust):

```sh
cargo build --release                   # → ./target/release/pwtune
```

**Runtime dependency:** PipeWire (`pactl`, `paplay`, `parecord`) — the default
audio server on most modern distros. Nothing else; the analysis (sweep, FFT/Welch,
EQ design) is built into the binary. Linux only (PipeWire).

## Commands

```sh
pwtune <command> [options]
```

| Command | What it does |
|---------|--------------|
| `measure` | Play a sweep, record it, print the frequency response. Changes nothing. |
| `create [name]` | Measure, then write a starting profile `./<name>.conf`. Prompts for name/boost/devices. |
| `list` | List the profiles in this folder (draft / calibrated) and their install state. |
| `install [name]` | Resolve the profile's target sink, deploy it, make it the default output. |
| `edit [name]` | Open a **draft** in `$EDITOR` (calibrated are frozen). |
| `delete [name]` | Delete a **draft** (calibrated are frozen). |
| `uninstall [name]` | Remove a deployed EQ — lists everything deployed (pwtune's + any other EQ config on the system). |
| `promote [name]` | Freeze a draft: rename `./<name>.conf` → `./<name>.calibrated.conf`. |

Every verb runs with no arguments too: it enumerates what's available, defaults to
the sensible choice for this machine, and prompts for the rest. Common options for
`measure`/`create`: `--speaker <sink>`, `--mic <source>`, `--iterations N`;
`create` also takes `--boost <dB>` (default `0`) and `--target-match <string>`.

## A typical session

```sh
cd ~/audio/profiles          # wherever you want the .conf files to live

pwtune measure               # see what the speaker actually does
pwtune create desk           # measure → ./desk.conf (prompts for boost, devices)
pwtune install desk          # deploy it, listen
pwtune edit desk             # tweak Gain/Freq/Q, then:
pwtune install desk          # re-deploy and re-listen
pwtune promote desk          # happy → freeze it as desk.calibrated.conf
git add desk.calibrated.conf && git commit -m "desk speaker EQ"
```

## How profiles work

Each profile is a PipeWire `filter-chain` config that creates a **virtual sink**
you select as your output; audio routes through the EQ before reaching the real
speaker.

- **Portable target.** Instead of a hardcoded sink, each profile carries a
  `# target-match:` line — matched against the sinks present at install time
  (`node.name`, and the monitor's EDID name via `node.nick`). Display-audio sink
  names differ per machine/GPU/bus, but a monitor's EDID name is stable, so one
  profile follows the device across machines.
- **Coexist.** Each profile gets its own node name, so several can be installed at
  once (e.g. a laptop EQ'ing both its internal speakers and an external monitor);
  switch between them in your sound settings.
- **Makeup gain is broadband and digital.** `--boost` adds shelf stages that lift
  the whole spectrum. It has a hard ceiling at 0 dBFS — past that it clips rather
  than getting louder, so for clean loudness use the speaker's/monitor's own
  volume. Raise/lower the `preamp` `Gain` values to taste.

## Notes / limitations

- Readings above ~1.5–2 kHz depend heavily on the mic and reflections — treat the
  high end as your ears' domain, not the analyzer's.
- Keep the room quiet and the mic close to (and pointed at) the speaker.
- The measurement passes through whatever EQ is currently active, so you can
  measure "before" (no profile installed) vs "after" to see the effect.

## History

Grew out of
[thinkpad-x1-carbon-pipewire-eq](https://github.com/jakobhviid/thinkpad-x1-carbon-pipewire-eq),
which fixed one laptop's speakers — now a general, folder-scoped tool for any
speaker, as a single self-contained Rust binary (no Python/NumPy/SciPy runtime).

## License

MIT
