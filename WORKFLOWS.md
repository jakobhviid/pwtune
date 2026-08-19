# pwtune workflows

End-to-end recipes for measuring a speaker and building, installing, and
maintaining a PipeWire EQ profile with `pwtune`. Written to be **driven by a
human or an LLM/agent**: every command has a flag path that skips the
interactive prompts, so an agent can run the whole lifecycle non-interactively.

## Model (read this first)

- **Folder-scoped.** `pwtune` acts on the `.conf` files in **whatever directory
  you run it in** — like a linter. There is no hidden library or home dir for
  profiles. `cd` to where you want the files to live (a scratch dir, a dotfiles
  repo) and every verb operates on *that* folder.
- **A profile's state is its filename**, nothing else:
  - `foo.draft.conf` — a **draft**: `edit`/`delete` work on it, tweak freely.
  - `foo.calibrated.conf` — **frozen**: `edit`/`delete` refuse it. `promote` is
    just the rename `foo.draft.conf → foo.calibrated.conf` that flips this lock.
  - Any other `.conf` in the folder is ignored — pwtune never touches a config
    it didn't create.
- **A profile is a virtual sink.** Each profile is a
  `libpipewire-module-filter-chain` config that creates an `effect_input.eq_<slug>`
  sink you select as your output; audio flows through the EQ and out to the real
  speaker (`effect_output.eq_<slug>`).
- **Installing is a two-directory move.** `create` writes the profile into the
  **current folder**; `install` copies a resolved copy into
  `~/.config/pipewire/pipewire.conf.d/pwtune-<name>.conf` (the "deploy dir"),
  restarts PipeWire if that file's contents changed, and makes it the default
  output. `uninstall` deletes that deployed file and restarts. Your folder copy is the source of truth; the deploy dir is
  derived and disposable.

## Convention for agents

The intended loop is **`measure` → `create` → `install` → *listen* → `edit` →
re-`install` → `promote`**. Two facts make this safe to automate:

- `measure` and `list` change nothing — call them freely to inspect state.
- Every verb takes a positional `NAME` and/or flags that pre-answer its prompts
  (see [Non-interactive driving](#non-interactive-driving-agents)). With those,
  no command blocks on a TTY.

The one thing an agent **cannot** fake is the acoustics: `measure`/`create` play
a real sweep and record it through a real mic. On a machine with no working
speaker+mic there is nothing to calibrate — drive `install`/`list`/`uninstall`
of an existing profile instead.

---

## W1 — Measure what a speaker actually does

Play a logarithmic sweep, record it back, print the frequency response. Averages
`--iterations` passes (default 3). Changes nothing — no files, no config.

```sh
pwtune measure                                   # prompts to pick speaker + mic
pwtune measure --speaker <sink> --mic <source>   # non-interactive
pwtune measure --iterations 5                    # more passes → steadier average
```

`<sink>`/`<source>` are PipeWire **`node.name`** strings — list them with
`pactl list short sinks` / `pactl list short sources`. Keep the room quiet and
the mic close to and pointed at the speaker. Measuring with **no profile
installed** gives you the raw "before"; measuring **through** an installed
profile shows the "after" — that's how you check an EQ's effect.

---

## W2 — Build a starting profile (`create`)

Measure, design a parametric EQ toward a flat target, and write
`./<name>.draft.conf` in the current folder.

```sh
cd ~/audio/profiles                              # where the .conf files should live
pwtune create desk                               # measure → ./desk.draft.conf (prompts for boost, class, devices)
pwtune create desk \
  --speaker <sink> --mic <source> \
  --boost 0 --target-match "DELL U2720Q" \
  --class bookshelf                              # fully non-interactive
```

- **`NAME`** — omit it and pwtune proposes one (a slug of the monitor's EDID
  name, else `my-speaker`) and prompts. The name becomes the file prefix, the
  `node.description`, and the virtual-sink slug.
- **`--boost <dB>`** (default `0`) — broadband digital makeup gain, one `preamp`
  shelf whose corner sits below the audible band so the lift is flat across it. It
  has a **hard ceiling at 0 dBFS**: past that it clips rather than getting louder.
  For clean loudness use the speaker's/monitor's own volume; use `--boost` only to
  match levels between profiles.
- **`--target-match <string>`** — the pattern stored in the profile that
  `install` later resolves to a real sink (see W3). If omitted, `create`
  defaults it to the speaker's EDID nick when known, else its `node.name`.
- **`--class <name>`** — how low the EQ may push: `laptop` (high-pass 150 Hz),
  `laptop-woofer` (100), `desktop` (80), `bookshelf` (50), `fullrange` (none). A
  class adds a 4th-order Butterworth high-pass at its corner and stops the designer
  placing any band below it.
- **`--highpass <Hz|none>`** — overrides the class corner outright. Pass a number
  when you know the driver's real limit, or `none` to leave the low end alone.
- **`--highpass-align <name>`** — `butterworth` (default, 3.01 dB down at the corner)
  or `linkwitz-riley` (6.02 dB down, passing less excursion energy below it). Recorded
  in the profile header.

### Why a class, and why you have to say

Every driver has a floor below which it moves air nowhere: the cone travels and
nothing is heard. Amplitude drives displacement, protection detects it, and protection
responds by reducing gain **broadband** — so at levels where protection is engaging,
removing content below the floor can leave a speaker audibly louder even though a
high-pass only subtracts. Quieter than that, it is simply quieter.

No magnitude sweep locates that floor, however good the mic, because the floor is a
**mechanical** limit: excursion, heat, and the protection's trip point. A driver's
−20 dB point is not its safe-operating point. So the corner is declared rather than
measured, and the per-class numbers are **conservative estimates**. `--highpass`
supplies a real one when you have it.

Declaring nothing declares no floor. An interactive `create` offers the detected
class as the Enter-default; a non-interactive one with no `--class` designs
unbounded rather than applying a corner nobody chose.

### What `create` reports

The response is recovered by deconvolving the sweep, which locates the playback delay
rather than assuming one, and only the *shape* is used — an uncalibrated mic cannot
give you absolute level. EQ bands then overlap, so a cascade delivers more than the sum
of the gains its bands request. `create` fits the gains against the response the chain
actually produces, holds the cascade's peak inside a budget, and prints:

- **the realised curve** — what the profile does, per octave centre;
- **peak chain gain and where** — the headroom the chain needs below 0 dBFS before a
  full-scale signal at that frequency clips;
- **bands pinned at the gain limit**, if any — the target there sits further away than
  one filter may travel, which on a small speaker is mostly the mic's own rolloff.

These are the only numeric quality signals available without listening, which makes
them the ones an agent should read.

The low / low-mid correction is measured and trustworthy; the top end is a guess
(the mic barely hears it). Treat the draft as a **starting point** to refine by
ear, not a finished result.

---

## W3 — Install a profile (`install`) and how target-match resolves

`install` finds the profile's `# target-match:` line, resolves it against the
sinks present **right now**, deploys the config, restarts PipeWire if the deployed
file changed, and sets the EQ as the default output. Re-installing an unchanged
profile leaves playback running, since reopening the PCM clicks on some amplifiers.

```sh
pwtune install desk                    # resolve target-match, deploy, set default
pwtune install desk --speaker "HDMI"   # override the stored target-match for this install
pwtune install                         # no name → pick from a list (draft/calibrated sections)
```

**How target-match resolution actually works** (this is the subtle part, and the
reason profiles are portable across machines):

- The stored string is compiled as a **case-insensitive regex** (`(?i)…`) and
  tested against the **entire `pactl list sinks` block** of each sink — not just
  `node.name`. So it can match the name, the `Description`, **or** the monitor's
  EDID `node.nick`. Because the EDID name is stable while display-audio
  `node.name`s differ per machine/GPU/bus, one profile follows the device across
  machines.
- It is a **regex**, so metacharacters are live: `.` matches any character, and
  an unbalanced `[` or `(` makes the pattern invalid — in which case it silently
  matches **nothing**. For a literal match, escape regex specials (a monitor
  model like `LG.HDR.4K` will match more loosely than you expect).
- pwtune's own `effect_input.` / `effect_output.` virtual sinks are excluded, so
  a profile never matches itself.
- **Zero matches** → error listing the connected sinks (`is that speaker/monitor
  connected?`). **Multiple matches** → you're prompted to pick the target sink;
  `--speaker <pattern>` narrows it ahead of time for non-interactive runs.

After install, the EQ sink (`effect_input.eq_<slug>`) becomes the default output
— play something to hear it.

---

## W4 — Tune, then freeze (`edit` → `install` → `promote`)

Iterate on a draft until it sounds right, then lock it.

```sh
pwtune edit desk        # opens ./desk.draft.conf in $EDITOR (falls back to nano)
pwtune install desk     # re-deploy the edited draft, listen
# …repeat edit/install until happy…
pwtune promote desk     # rename → ./desk.calibrated.conf (frozen)
```

- `edit` opens the draft in `$EDITOR`; tweak the per-band `Freq` / `Q` / `Gain`
  and the `preamp` gains. `edit`/`delete` **refuse a calibrated profile** — to
  change a frozen one, re-measure into a new draft and promote again.
- `promote` is a pure rename that sets the frozen flag. If a
  `<name>.calibrated.conf` already exists it asks before overwriting (default
  **no**). Commit the file yourself wherever you keep it.
- `delete <name>` removes a **draft** after a `y/N` confirm (default no); it
  warns first if that profile is currently installed.

---

## W5 — See the lay of the land (`list`)

One read-only line per profile in the current folder, with live state.

```sh
pwtune list
#   ● desk        Desk speakers            [calibrated · installed · active output]
#   ○ tv          Living-room TV           [draft]
```

- **`●` / `○`** — whether the profile's target-match resolves to a sink that is
  **connected right now**.
- **Tags** — `draft` or `calibrated`; the `speaker-class` and high-pass corner the
  profile records; `installed` if a `pwtune-<name>.conf` exists in the deploy dir;
  `active output` if the EQ sink is the current default. Scope your next action from
  these.
- **Orphans.** `list` also names any `pwtune-*.conf` in the deploy dir with no
  profile in this folder. PipeWire loads every config it finds, so such a chain is
  still loaded and still offered as an output — expected if you installed it from
  another folder, worth clearing with `uninstall` if its source was renamed or
  deleted. `list` reports them and changes nothing: the deploy dir is machine-wide
  while pwtune is folder-scoped, so pruning from here would delete another folder's
  work.

---

## W6 — Remove a deployed EQ (`uninstall`)

`uninstall` is **system-wide**: it lists everything EQ-like in the deploy dir,
split into two buckets, and removes the one you pick (then restarts PipeWire).

```sh
pwtune uninstall desk    # remove pwtune-desk.conf
pwtune uninstall         # pick from the list
```

- **Installed by pwtune** — `pwtune-*.conf` files, labelled by profile name.
- **Other EQ configs on this system (not from pwtune)** — any other
  filter-chain config with an `effect_input` (a leftover, or one another tool
  dropped). pwtune can't tie these back to a source folder, so they have no
  draft/calibrated tier — but it can still clean them up.

Uninstalling only touches the deploy dir; your `.conf` in the working folder is
untouched, so `install` can redeploy it later.

---

## Non-interactive driving (agents)

Which flags eliminate every prompt, per command:

| Command | Fully non-interactive with… | Notes |
|---------|-----------------------------|-------|
| `measure` | `--speaker <sink> --mic <source> [--iterations N]` | needs real audio hardware |
| `create` | `NAME --speaker --mic --boost --target-match --class [--highpass --highpass-align]` | needs real audio hardware; without `--class` **or** `--highpass` a non-interactive run adds no filter and places no band below 120 Hz |
| `install` | `NAME --speaker <pattern>` | prompts only if `<pattern>` resolves to **>1** sink |
| `edit` | `NAME` | still opens `$EDITOR` — interactive by nature |
| `promote` | `NAME` | asks only if the calibrated file already exists |
| `delete` | `NAME` | always asks `y/N` (default no) |
| `uninstall` | `NAME` | no confirm — removes immediately |

**Confirmation prompts read stdin, and EOF counts as the default.** So piping
`</dev/null` makes `delete`/`promote`-overwrite safe no-ops (they keep the file),
while `uninstall <name>` proceeds unattended. To *force* a delete non-interactively
you must actually feed a `y` (`echo y | pwtune delete foo`).

Environment knobs an agent can rely on:

- **`--quiet` / `-q`** — essential result + errors only; suppresses the `▸` info
  chatter and the `Next:` hint block. `--verbose` / `-v` for the opposite.
- **`NO_COLOR=1`** — disables ANSI color (also auto-off when stdout isn't a TTY),
  so captured output stays clean.
- **`PWTUNE_LAUNCHER`** — the command name printed in the `Next:` hints; set it if
  you wrap `pwtune` behind another entrypoint so the suggestions stay copy-pasteable.
- **`EDITOR`** — used by `edit` (falls back to `nano`).
- **`--llm`** — prints this whole guide (command reference + README + these
  workflows) to stdout; the machine-readable entry point.

---

## Profile anatomy & manual deployment

A profile is a plain PipeWire filter-chain config you can read and hand-edit. The
parts that matter:

```conf
# target-match: DELL U2720Q          ← the regex install resolves (W3)
# speaker-class: bookshelf           ← what bounded the design
# highpass: 50                       ← corner of the 4th-order Butterworth, when present
…
capture.props  = { node.name = "effect_input.eq_desk"  media.class = Audio/Sink … }   ← the sink you select as output
playback.props = { node.name = "effect_output.eq_desk" node.target = "__TARGET_SINK__" … }  ← the real speaker
```

`install` replaces the `__TARGET_SINK__` placeholder with the resolved sink
`node.name`. To deploy **by hand** instead: replace `__TARGET_SINK__` with a name
from `pactl list short sinks`, drop the file into
`~/.config/pipewire/pipewire.conf.d/`, and
`systemctl --user restart pipewire pipewire-pulse`.

- **Coexistence.** Each profile's node name carries its own `<slug>`, so several
  can be installed at once (e.g. one EQ for laptop speakers, one for an external
  monitor); switch between them in your sound settings.
- **One install path per profile per machine.** Installing the *same* profile via
  two tools (e.g. `pwtune` **and** a provisioning script) drops two files whose
  filter chains share a node name — PipeWire will conflict. Pick one path per
  machine.
- **Idle suspension is off.** Generated profiles set
  `session.suspend-timeout-seconds = 0` on both ends. WirePlumber's shipped suspend
  rule matches `alsa_input.*` / `alsa_output.*` and is applied through the ALSA
  monitor, which never sees a filter-chain node, so without this these nodes idle out
  after 5 seconds and some amplifiers click when the PCM reopens. Where a distro ships
  that rule the ALSA node is already pinned, so the extra cost is the graph's own
  wakeups.

---

## Ready-made profiles

Device-specific profiles live in
[pipewire-speaker-profiles](https://github.com/jakobhviid/pipewire-speaker-profiles).
Drop one into a folder and `pwtune install` it (or deploy by hand as above), and
send finished ones back there via PR.

## Limitations

- Readings above ~1.5–2 kHz depend heavily on the mic and room reflections —
  treat the high end as your ears' domain, not the analyzer's. Trust the
  low / low-mid bands.
- The auto-EQ is a **calibration aid, not a magic button**: it gets you a sane
  starting curve, then you refine by ear and re-measure.
- The high-pass corner is **declared, not measured** — it is a mechanical limit, which
  no magnitude sweep shows at any microphone quality. A class is a conservative
  estimate; `--highpass` takes a real number when you have one.
- `install` restarts PipeWire only when the deployed config differs from what is
  already there, so re-installing an unchanged profile does not interrupt playback.
