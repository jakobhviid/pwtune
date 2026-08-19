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

Ready-made profiles for specific devices live in
**[pipewire-speaker-profiles](https://github.com/jakobhviid/pipewire-speaker-profiles)**
— install them with `pwtune` or by hand, and send finished ones there via PR.

## How it works: it operates on the current folder

`pwtune` works on the `.conf` files in **whatever directory you run it in** —
like a linter or formatter. There's no hidden library or home directory. `cd` to
wherever you want your profiles to live (a scratch dir, a dotfiles repo, …), and
the tool acts on that folder.

A profile is "calibrated" (frozen) purely by its **filename**:

- `desk.draft.conf` — a **draft**: editable, deletable, tweak away.
- `desk.calibrated.conf` — **calibrated**: frozen. `edit`/`delete` refuse it.

`promote` is just a rename (`desk.draft.conf` → `desk.calibrated.conf`) — its only job
is that lock. You commit the files yourself, wherever you keep them.

## Install

**Homebrew** (Linux, x86_64 / arm64) — pours a prebuilt bottle on x86_64, so no
compiler/build tools are needed:

```sh
brew install jakobhviid/tap/pwtune
```

**Or paste one line** — no Homebrew, no compiler, no root (installs to
`~/.local/bin`):

```sh
curl -fsSL https://raw.githubusercontent.com/jakobhviid/pwtune/main/install.sh | sh
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
| `create [name]` | Measure, then write a starting profile `./<name>.draft.conf`. Prompts for name/boost/class/devices. |
| `list` | List the profiles in this folder (draft / calibrated) and their install state. |
| `install [name]` | Resolve the profile's target sink, deploy it, make it the default output. |
| `edit [name]` | Open a **draft** in `$EDITOR` (calibrated are frozen). |
| `delete [name]` | Delete a **draft** (calibrated are frozen). |
| `uninstall [name]` | Remove a deployed EQ — lists everything deployed (pwtune's + any other EQ config on the system). |
| `promote [name]` | Freeze a draft: rename `./<name>.draft.conf` → `./<name>.calibrated.conf`. |

Every verb runs with no arguments too: it enumerates what's available, defaults to
the sensible choice for this machine, and prompts for the rest. Common options for
`measure`/`create`: `--speaker <sink>`, `--mic <source>`, `--iterations N`;
`create` also takes `--boost <dB>` (`0`–`30`, default `0`), `--target-match <string>`,
`--class <name>`, `--highpass <Hz|none>`, and `--highpass-align <name>`.

## A typical session

```sh
cd ~/audio/profiles          # wherever you want the .conf files to live

pwtune measure               # see what the speaker actually does
pwtune create desk           # measure → ./desk.draft.conf (prompts for boost, devices)
pwtune install desk          # deploy it, listen
pwtune edit desk             # tweak Gain/Freq/Q, then:
pwtune install desk          # re-deploy and re-listen
pwtune promote desk          # happy → freeze it as desk.calibrated.conf
git add desk.calibrated.conf && git commit -m "desk speaker EQ"
```

## Speaker class: how low to push

Correcting a response means asking the driver for more output somewhere, and every
driver has a floor below which it moves air nowhere: the cone travels and nothing is
heard. Amplitude drives displacement, protection detects it, and protection responds by
reducing gain *broadband* — which is why, **at levels where protection is engaging**,
removing content below the floor can leave a speaker audibly louder even though a
high-pass only subtracts. Below those levels it is simply quieter.

No magnitude sweep can find that floor, however good the microphone, because the floor
is a **mechanical** limit — excursion, heat, and the trip point of the protection in
front of the driver. A driver's −20 dB point is not its safe-operating point. So
`create` asks you to declare it, from something you can see:

| `--class` | what it looks like | high-pass |
|-----------|--------------------|-----------|
| `laptop` | ultrabook or tablet, no separate woofer grilles | 150 Hz |
| `laptop-woofer` | larger chassis with dedicated woofers or quad speakers | 100 Hz |
| `desktop` | small powered speakers or a soundbar, 2–3″ driver | 80 Hz |
| `bookshelf` | 5″ woofer or larger | 50 Hz |
| `fullrange` | floorstanders, monitors, or any system with a subwoofer | none |

A class does two things: it adds a 4th-order high-pass at that corner, and it stops the
EQ placing any band within half an octave above it — a boost that close only fights the
filter. `--highpass-align` picks the alignment: `butterworth` (the default, 3.01 dB down
at the corner) or `linkwitz-riley` (6.02 dB down, passing less excursion energy below
it).

**The corners are conservative estimates, not measurements.** If you know your
driver's real limit — a datasheet, a vendor tuning file, your own ears — pass
`--highpass <Hz>`, which overrides the class; `--highpass none` leaves the low end
alone. Naming no class declares no floor, which is right for anything that genuinely
reaches low and wrong for a laptop, so pick one when it matters. A non-interactive
run with no `--class` declares no floor rather than guessing at one.

## How profiles work

Each profile is a PipeWire `filter-chain` config that creates a **virtual sink**
you select as your output; audio routes through the EQ before reaching the real
speaker.

- **Portable target.** Instead of a hardcoded sink, each profile carries a
  `# target-match:` line — a case-insensitive **regex** matched at install time
  against each sink's full `pactl list sinks` block, so it can hit the
  `node.name`, the description, or the monitor's EDID name (`node.nick`).
  Display-audio sink names differ per machine/GPU/bus, but a monitor's EDID name
  is stable, so one profile follows the device across machines. (Because it's a
  regex, escape special characters for a literal match — see `WORKFLOWS.md`.)
- **Coexist.** Each profile gets its own node name, so several can be installed at
  once (e.g. a laptop EQ'ing both its internal speakers and an external monitor);
  switch between them in your sound settings.
- **One install path per profile per machine.** Installing the *same* profile via
  two different tools (e.g. `pwtune` and a separate provisioning script) drops two
  files whose filter chains share a node name — PipeWire will conflict. Pick one
  install path per machine.
- **Makeup gain is broadband and digital.** `--boost` adds `preamp` shelves whose
  corners sit below the audible band, so they lift everything you can hear by the
  amount asked for. PipeWire accepts at most +20 dB on one control and clamps quietly
  past that, so the gain is split across as many shelves as it takes. It also has a
  hard ceiling at 0 dBFS — past that it clips rather than getting louder, so for clean
  loudness use the speaker's/monitor's own volume.

## Notes / limitations

- Readings above ~1.5–2 kHz depend heavily on the mic and reflections — treat the
  high end as your ears' domain, not the analyzer's.
- Keep the room quiet and the mic close to (and pointed at) the speaker.
- The measurement passes through whatever EQ is currently active, so you can
  measure "before" (no profile installed) vs "after" to see the effect.
- EQ bands overlap, so a chain delivers more than the sum of the gains its bands
  request. `create` fits the gains against the response the cascade actually produces,
  holds its peak inside a budget, prints the realised curve, and reports that peak —
  the headroom the chain needs below 0 dBFS before a full-scale signal clips.
- The response is recovered by deconvolving the sweep, so the measurement finds the
  playback delay itself rather than assuming one. Only the *shape* is used: an
  uncalibrated mic cannot give you absolute level.
- A band pinned at its gain limit is reported. That means the target sits further
  away than one filter may travel, and on a small speaker most of that gap is the
  mic's own rolloff rather than anything worth correcting.

## History

Grew out of
[thinkpad-x1-carbon-pipewire-eq](https://github.com/jakobhviid/thinkpad-x1-carbon-pipewire-eq),
which fixed one laptop's speakers — now a general, folder-scoped tool for any
speaker, as a single self-contained Rust binary (no Python/NumPy/SciPy runtime).

## AI disclosure

Parts of this codebase were written with the assistance of AI coding agents
(Claude Code, opencode, and others). All changes were reviewed by the maintainer.

## License

MIT
