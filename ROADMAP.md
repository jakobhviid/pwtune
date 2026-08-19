# pwtune roadmap

Work that is understood but not done, and the decisions behind the parts that are.
Written to be picked up cold: each item says what the problem is, what would settle
it, and what has already been ruled out, so the reasoning does not have to be
rediscovered.

## Read this first: the tuning defaults rest on no evidence above 2 kHz

**Every tuning judgement in this repository was chosen against measurements that were
invalid in their top three octaves** — the target curve, the class corners, the
smoothing width, the band plan. None of them is known to be wrong. None is known to be
right either. They were picked when the analysis compared the reference and the
recording at fixed offsets, so above roughly 2 kHz it divided sweep energy by room
noise, and a bit-perfect recording read −87.6 dB at 8 kHz.

The measurement itself is sound now — `an_undistorted_recording_measures_flat` and
`a_known_filter_is_recovered_from_its_own_sweep` hold it to that — but no measurement
made with it has been compared against a known-good result yet.

So the first move on resuming is a fresh sweep with a mic that can be trusted (on the
reference machine, the boom mic rather than the internal DMIC), checked against the
deployed profile. Several items below are gated on that and say so.

## Open work

### Re-measure and re-derive the tuning defaults
Gated on a trustworthy measurement. Once one exists:

- **Target curve.** `TARGET` in `src/dsp.rs` rises to +6 dB at 50 Hz with its knee at
  800 Hz, so it is a broad tilt through 200–500 Hz — the boxy region of every laptop
  and small sealed cabinet, and where a chassis resonance already sits. Room-target
  convention puts a bass-shelf knee at 100–120 Hz and stays flat above. Asking for
  +5 dB at 100 Hz while a 100 Hz high-pass is declared is incoherent, so the lift
  should be suppressed below a declared corner. The −1 dB dips at 2.5 k and 16 k are
  false precision: no band sits at 2500 Hz and the fit's own between-centre error is
  larger. Independent of re-measurement, `interp_target` interpolates linearly in
  frequency rather than log-frequency, worth ~0.6 dB of shape error near 300 Hz and
  1.1 kHz.
- **Class corners.** Only `laptop-woofer`'s 100 Hz has real grounding (vendor tuning
  data for one chassis). The other four are physics-and-convention priors. They are
  conservative — all sit below the likely driver fs, so they under-protect rather than
  over-protect, which is the safe direction — but they are guesses and are documented
  as such.
- **High-pass alignment.** Settled, and recorded here so it is not reopened without
  new information. `--highpass-align` offers `butterworth` (default) and
  `linkwitz-riley`; the default stays Butterworth.

  Linkwitz-Riley exists to solve a crossover problem that a protective high-pass does
  not have. Its defining property — two cascaded identical Butterworth sections, 6 dB
  down at the corner — is what makes a complementary low-pass and high-pass sum to
  flat, in phase, through a crossover region. A protective filter has nothing summing
  with it, so that virtue is inapplicable, and "LR is standard for crossovers" does
  not transfer. For a standalone filter the conventional choice is Butterworth,
  because maximally-flat passband is the right criterion when nothing sums with it.

  What the literature is firm about is *order*, not alignment: below fs a sealed
  driver's displacement per volt is constant, so 12 dB/octave merely cancels the
  excursion rise and you need steeper to reduce it — which is why 24 dB/octave is the
  standard subsonic filter. Both candidates are 4th order, so that criterion is
  already met. Beyond it, the variable that governs protection is corner placement
  relative to fs and Xmax, which is what `--class` and `--highpass` set.

  The two are not corner-shifts of each other: the closest Butterworth match to LR4 at
  100 Hz is 106 Hz and still 2.1 dB out, because LR4 has a broader, earlier knee —
  more attenuation near the corner, slightly less far below it. Pink-weighted, LR4
  passes about 43% less sub-corner energy (−2.45 dB), constant across corners, and the
  two agree within 0.5 dB above twice the corner. So the whole difference is roughly
  one octave wide, and bending the knee to buy 2.45 dB is compensating in the wrong
  variable when the corner is right there and is measurable.

  LR4 remains available and is the right choice where vendor data specifies it, as it
  does for the ThinkPad chassis this grew out of — though that filter sits inside a
  full dynamics chain rather than standing alone.

### Bound the correction range at the top by measurement quality
The designer places bands wherever the plan allows, with no check that the
measurement supports one there. A gate is still wanted, with hard constraints:

- SNR may only **suppress** bands, never add a filter. Never derive a low-pass from
  it. A high-pass removes acoustic content, so it is a claim about the driver and must
  be declared; suppressing a band leaves the response untouched and needs only a claim
  about the measurement. A low-pass at the mic's limit would delete treble the speaker
  genuinely produces.
- The gate reaches globally through the anchor, since the anchor is a mean over the
  emitted bands. Compute it from gated bands only, and print it.
- Distinguish a **cliff** in SNR against frequency from a **gradual decline**. No
  microphone dies at 2 kHz; a cliff means a timing or truncation fault, and should be
  an error, not a silent suppression. A gate built before the alignment fix would have
  hidden that bug rather than exposing it — it silently vanishes bands, and the
  conclusion drawn was "this mic is bad".
- Per-bin gating from 0.5 s of lead-in silence is noisy (~4 Welch segments). Gate on
  1/3-octave bands to match `band_mean`, or lengthen `SETTLE`.
- Cheaper and stronger signal already available: `--iterations` runs several passes
  and `average()` discards the variance. Per-bin standard deviation across passes
  catches coherent errors an SNR gate passes cleanly — capture-side compression,
  stream contamination, glitched captures, drift. Gate on the worse of the two.
- Report it as the **measurement** range, never the speaker range.

### Microphone calibration
The real fix for the identifiability problem below. Accept a calibration file —
the two-column `.frd`/`.cal` format REW and ARTA use, which a UMIK-1 ships with — and
subtract it from the measured curve. Generic, standard, and it makes a boom-mic
workflow first-class. Does nothing for an uncalibrated laptop mic.

### Measuring the driver's floor
The high-pass corner is declared because the floor is a mechanical limit — excursion,
heat, the protection trip point — that no magnitude sweep shows at any mic quality.
Two experiments can measure it, and both are ruled *in*:

- **Two-level sweep.** Sweep at two levels; in the linear region the curves differ by
  the level difference at every frequency, and diverge where the system compresses.
  Self-referencing, so mic response cancels exactly. Fails safe: capture-side AGC
  compresses both sweeps and shrinks the divergence, so it under-detects rather than
  reporting a wrong corner. Costs one extra sweep.
- **Distortion against frequency.** More direct, slower, louder, and harmonics land
  where the mic responds differently.

Neither separates mic from speaker — see below — but neither needs to, because both
measure *nonlinearity* rather than magnitude.

### Deployment
- **Deploy to `filter-chain.conf.d` and restart only that unit.** PipeWire ships
  `filter-chain.service` for user DSP; the config body is byte-identical, and
  restarting that unit alone leaves the daemon, the hardware PCM, WirePlumber and
  every other stream alive. This is a better answer to the resume click than skipping
  restarts, and it makes most of the current restart logic unnecessary. Needs
  `systemctl --user enable --now filter-chain` on first install, and both directories
  scanned during a transition. Not done because it cannot be validated without
  restarting audio.
- **Settle poll instead of a fixed sleep.** `restart_pipewire` sleeps 1500 ms and
  discards `systemctl`'s exit status, and the "is the chain loaded?" check runs after
  it. On a slow machine that check runs too early and either fires a second full
  restart — the click the logic exists to avoid — or reports "couldn't set default"
  when nothing loaded. Poll for the node for a few seconds instead, check
  `systemctl`'s status, and distinguish "the module failed to load" from "couldn't set
  the default" so the user is pointed at the config rather than at their desktop.
- **Match the sink's channel map.** The graph is stereo and PipeWire replicates it per
  channel, so installing on a 5.1 sink downmixes surround. `install` warns about this;
  emitting a matching `audio.channels`/`audio.position` is the real fix. Deferred
  because the position token list cannot be verified where every sink is stereo, and a
  wrong channel map loads a chain that plays **silence** with only a journal line —
  the worst failure mode this tool has.

### Diagnostics
- **Scan all three config levels.** PipeWire loads `/usr/share`, `/etc` and the user
  directory with same-name shadowing, each fragment's `context.modules` in addition to
  the base. A system EQ chain in `/etc/pipewire/pipewire.conf.d/` is invisible to
  `list` and undeletable by `uninstall`, which still claims audio goes straight to the
  speaker. Scan all levels read-only; only ever delete from the user level.
- **`Kind::Managed` trusts a filename.** It is decided purely by the `pwtune-` prefix
  with no content check, so a file a user happens to name that way is offered for
  deletion as pwtune's.
- **`--device` is a best-effort bypass.** If the named target cannot link, WirePlumber
  falls through to the default-target hooks — into the effect sink — unless
  `node.dont-fallback` is set. A sweep meant for the raw sink can therefore be measured
  *through* the EQ under test. Set `PIPEWIRE_PROPS='{ node.dont-fallback=true }'` on
  the `paplay`/`parecord` invocations and verify with `pw-link -l`. Needs audio to
  validate.

## Decisions already taken

Recorded so they are not re-litigated. Each was considered and rejected or chosen for
a stated reason.

- **The correction is anchored on loudness neutrality, not cut-only.** Cut-only
  (choose the offset so the maximum correction is 0 dB) is standard practice and
  headroom-safe by construction, but it fails here: with an unfillable ~20 dB deficit
  inside the corrected range it cuts the mids by ~20 dB to match bass the driver still
  cannot produce. Mean-centring minimises the sum of squares, so it minimises how far
  any band is driven and how often the clamp bites. Any constant offset is level, not
  shape, and `--boost` plus the speaker's own volume set level.
- **The realised cascade peak has a budget, applied separately from the per-band
  clamp.** Overlapping bands add, so a per-band clamp never bounded what the chain
  delivers.
- **The makeup-gain corner stays at 10 Hz.** Measured as maximum magnitude error over
  20 Hz–20 kHz, f32 conditioning is 0.012–0.048 dB for every candidate corner —
  inaudible, so conditioning does not decide it. Makeup gain should be flat across the
  audible band, and protecting the driver is the high-pass's job. One job per stage.
- **Every band peaks; none shelves.** A shelf holds its gain past its corner without
  bound and reaches only half its stated gain at the corner itself. At the bottom that
  buys excursion; at the top it extrapolates a deficit measured at 10 kHz out to
  20 kHz. (A low shelf *is* the right shape for a driver rolling off below its
  passband — but only behind a declared high-pass, which is a change to the band plan,
  not a reversal of this one.)
- **`pwtune` does not touch ALSA capture controls.** Disabling the capture DRC and the
  fixed beamformer on the reference machine changed nothing above 1 kHz; the effect
  blamed on them was the alignment bug. Mutating unknown mixer state on unknown cards,
  with restore-on-crash semantics, is a large blast radius for something that was not
  the problem.
- **Orphaned deployed profiles are reported, never pruned.** The deploy directory is
  machine-wide while pwtune is folder-scoped, so deleting on the assumption that this
  folder is the only source would remove another folder's work.

## Known unresolvable

Separating microphone response from speaker response needs a known-flat element on one
side. With one unknown mic and one unknown speaker there is one equation and two
unknowns; this is an identifiability limit, not a missing feature. The design works
around it rather than pretending otherwise: the low end is **declared** (subtractive,
so it needs a claim about the driver) and the top end **abstains** (neutral, so it
needs only a claim about the measurement). Mic calibration resolves it for anyone who
has a calibrated mic, and for nobody else.
