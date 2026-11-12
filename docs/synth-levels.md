# Synth output levels

The OpenSynth voice path carries a headroom contract: a patch whose every
level-raising stage sits at its maximum still peaks below **-6 dBFS** at unity
output. Each stage that can raise the level therefore carries its own
normalisation, and the master output has a final ceiling so the sum of those
stages cannot leave the instrument above the contract. The contract is what
lets a preview start without rendering and measuring the figure first; the
instrument measures each patch once, for loudness, on its own.

## The contract

- **Unit:** peak sample magnitude of the instrument output, at `output.gain =
  1.0`, with the track at 0 dB.
- **Ceiling:** `OUTPUT_CEILING = 0.49` (-6.20 dBFS). The master stage is
  transparent up to `OUTPUT_CEILING_KNEE = 0.441` (-7.11 dBFS) and then follows
  a C1 exponential segment that approaches the ceiling without reaching it. A
  patch below the knee keeps its exact waveform.
- **Test:** `crates/oc-synth-core/src/synth/levels.rs`
  (`worst_case_matrix_stays_below_the_output_ceiling`).
- **Report:** the same module's ignored `report_stage_levels` prints the tables
  quoted below.

The ceiling is a static, memoryless law, not a measurement, so the preview does
not render the figure to measure its peak. See [Audition preview](#audition-preview).

## Per-stage audit

Levels are measured at `output.gain = 0.1` unless the row says unity, so the
master ceiling stays transparent and the reading describes the stage itself.
Every number is a peak in dBFS over a note or chord held for one to two
seconds, 48 kHz.

| Stage | Raises the level? | Law that bounds it | Measured worst case |
|---|---|---|---|
| Wavetable unison (osc A/B/C) | Yes: the stack's coherent peak grows with the voice count | Per-voice gain is a measured level table over count and detune, so the stack renders at one voice's RMS; it is not capped at `1/sqrt(N)`, because a partly coherent stack sums above power | 1 voice -20.13, 2 -19.53, 4 -16.45, 8 -17.04, 16 -16.15 (osc A, 17.1 ct); RMS flat within 0.7 dB over the same sweep |
| Sampler unison (sample layers) | Yes: the old law was `N^0.68`, +16.4 dB at 16 voices | `1/sqrt(N)`, the incoherent normalisation; total power flat | 1/2/4/8 voices within 0.5 dB on a sine (`unison_law_keeps_the_mono_level_flat`) |
| Filter resonance (SVF, SVF3, SVF4) | Yes: the two-pole peak adds `1 / damping` at the corner, up to +18 dB | Damping is floored at `1 / MAX_RESONANT_GAIN` with `MAX_RESONANT_GAIN = 2.0`, so the peak gain never exceeds +6 dB; the passband is untouched | Peak at the corner minus the passband ≤ +6.0 dB over res 0..1 (test `resonance_peak_stays_inside_the_stage_budget`) |
| Filter resonance (Ladder, DirtyLadder, Moog taps) | Yes, in principle | The ladder's saturator bounds the loop and the Moog input gain `1 + feedback` restores the passband; measured peak rise is small, so no extra law | Moog12 +3.9 dB from res 0 to 1; Ladder and DirtyLadder peak fall as resonance rises |
| Filter drive | Yes: the input gain is `(1 + drive)^4`, +24 dB at full | The cubic saturator clamps its input at ±1.5 and its output at 1.0, so the stage's peak cannot pass 0 dBFS; the master ceiling bounds the patch | Drive sweep at unity peaks -6.20 dBFS (`drive_input_sweep_stays_bounded`); at 0.1 gain drive 1 raises RMS by 5.4 dB and peak by 3.5 dB |
| Warp modes | Yes, slightly: the distortion family adds harmonics that raise the peak | The shaper clamps and the master ceiling | Largest raise +2.2 dB (`DistSoftClip` at full amount); phase and readout modes stay within ±1 dB; PWM and quantize at full amount render silence |
| Compressor make-up | Yes: `makeup` reaches +24 dB | `makeup` and the band gains are clamped to ±12 dB, and the module's own lookahead limiter holds its output at -0.3 dBFS | At 0.1 gain the module at full make-up peaks -14.28 dBFS |
| Compressor band gain | Yes: ±24 dB | Same ±12 dB clamp | Included above |
| Multiband make-up | Yes: global `makeup` plus three band make-ups | Global make-up clamped to +12 dB; band make-ups and band gains clamped to ±12 dB | At 0.1 gain the module at full make-up peaks -6.20 dBFS |
| Reverb, delay, chorus, phaser, flanger wet | Yes: a wet path adds energy and `level_out` reaches +6 dB | `mix` is bounded 0..1, `level_out` 0..2 by the palette; the master ceiling bounds the sum | Each module alone at 0.1 gain peaks at or below -6.20 dBFS (reverb, chorus, phaser and flanger reach the ceiling; delay -11.13) |
| EQ3 bands | Yes: ±18 dB per band, three bands | Not normalised: the bands are a deliberate tone control. The master ceiling bounds the module | Full boost at 0.1 gain reaches the ceiling (-6.20 dBFS) |
| Polyphony summing | Yes: the voice sum is a plain sum | No per-voice normalisation: a dynamic `1/sqrt(N)` law would change every chord patch's level. The master ceiling is the normalisation for this node | 1..16 distinct notes at unity all peak -6.20 dBFS; RMS rises 2.15 dB from one to sixteen notes |
| Macro extremes | Yes, through their destinations | Macros are sources, not gains. Their level destinations are `VoiceAmp` (clamped to `MAX_VOICE_AMP = 2.0`, +6 dB) and the FX gains (capped above) | Four macros at full into `VoiceAmp` at unity peak -6.20 dBFS |
| Master output | Yes: `output.gain` reaches 1.0 | `OUTPUT_CEILING` (see [The contract](#the-contract)) | All matrix cases -6.20 dBFS |

### The wavetable unison level table

The per-voice gain is a coarse measured table: eight voice counts by four
detune spreads, each cell the amplitude factor that lands the stack on one
voice's steady RMS. It is measured on one oscillator at C5 with the filter
bypassed, the stereo width at 0 and the patch trim pinned to 0 dB, and it is a
level law, not a character curve. The solver interpolates linearly between
cells.

The table is deliberately not capped at the incoherent normalisation
`1/sqrt(N)`. Voices that beat apart sum as power, but voices that only partly
beat sum above it: the four-voice, six-cent cell sits 1.14 dB over `1/sqrt(N)`,
and pinning the law to that normalisation is what left the detuned stacks up to
3 dB below one voice. What the table holds is the level. Every cell renders
within 1 dB of one voice at the measurement pitch, and a cell between the rows
and columns within 1.5 dB.

The sample-layer stack is a separate law: the incoherent `1/sqrt(N)`. A
`N^-0.32` law would make the total grow as `N^0.68` (+16.4 dB at sixteen
voices).

## Worst-case matrix

Each patch maximises one stage and renders eight distinct notes at unity
output. The contract test requires every peak below -6 dBFS.

| Case | Peak |
|---|---|
| Max unison on all three oscillators, every source at full | -6.20 dBFS |
| Max resonance, sine at the corner | -6.20 dBFS |
| Max drive | -6.20 dBFS |
| Both filters at max resonance, parallel | -6.20 dBFS |
| Every FX at its loudest, in series | -6.20 dBFS |
| Parallel rack at full branch gain | -6.20 dBFS |
| Warp at full amount on all three oscillators | -6.20 dBFS |
| Every stage at once | -6.20 dBFS |

## Factory bank, before and after

The bank probe (`review_factory_preview_levels` in
`crates/oc-gui/frontend/src-tauri/src/audition.rs`) renders all 62 patches
through the audition figure. Before the laws, 23 patches peaked above -6 dBFS
and the preview trimmed them; after, none does.

| Measurement | Before | After |
|---|---|---|
| Patches above -6 dBFS | 23 of 62 | 0 of 62 |
| Loudest patch | Filter Cry, -3.14 dBFS | Cinema Swell, -6.21 dBFS |
| Filter Cry | -3.14 dBFS (trim -2.86 dB) | -9.21 dBFS |
| Filter Cry RMS (three-note probe) | -19.55 dBFS | -21.62 dBFS |
| Growl Engine | -4.80 dBFS | -9.21 dBFS |
| Growl Engine RMS | -20.17 dBFS | -20.41 dBFS |
| Deep Space | -3.83 dBFS | -6.21 dBFS |

No patch got louder. The largest change is Filter Cry at -6.07 dB peak and
-2.07 dB RMS: its two resonant band-passes at Q 0.5 and 0.35 lose the resonant
excess that the +6 dB cap removes, and the master ceiling takes the rest.
Growl Engine loses 4.41 dB of peak but only 0.24 dB of RMS, so its loudness is
unchanged and only the isolated peak is bounded. Patches whose resonance sits
at or below 0.316 and whose output stays below the knee are unchanged.

## Per-patch loudness

The headroom contract bounds the peak, and a peak ceiling alone cannot make two
patches comparable: a sustained unison stack and a short pluck can share a peak
and sit eleven decibels apart in RMS. OpenSynth therefore normalises each
patch's own loudness, in the instrument, and every render path shares the law.

- **Probe.** A one-track project plays a fixed figure — the audition's six
  ascending eighths over one octave at 140 BPM — through the resolved preset at
  48 kHz. The integrated RMS of that render is the patch's level.
- **Law.** `gain_db = REFERENCE_RMS_DB - rms_db`, clamped to ±12 dB.
  `REFERENCE_RMS_DB = -23.8`, the measured median of the factory bank's 62
  patches (-23.82 dBFS RMS), so the median patch keeps its level and only the
  outliers move.
- **Register.** The figure moves down two octaves for a `filter.cutoff` below
  500 Hz and one octave below 1200 Hz, so a sub-bass patch is probed where it
  lives. The law never compensates key tracking: a dark patch played high stays
  quiet.
- **Cache.** The trim is keyed on the preset's source, plugin, program, file,
  full parameter list and effect chain. Two patches that share parameters and
  differ by one effect get their own measurement. Eligible self-contained
  patches also reuse exact calibration results across launches.
- **Application.** The trim rides in the built-in parameter path as
  `output.loudness_trim_db` and scales the patch's master gain when the patch is
  loaded. The document is untouched: `output.gain` and the DSL source stay as
  written. Offline renders, streaming playback and the preset browser all read
  the same parameter list, so the preview needs no case of its own.
- **Live keys.** The patch editor prepares the trim after preset selection or
  edits settle and starts with a ready trim when available. For an eligible
  native patch with a cold cache, it plays immediately at the
  authored level and measures in the background. The gain changes only after
  held notes and effect tails finish. A live patch edit supersedes a pending
  measurement for that audition.
- **Scope.** Only OpenSynth patches are measured. SoundFont and hosted-plugin
  instruments keep their own level.

## Audition preview

`audition.rs` does not render the figure to measure it. `prepare` and
`prepare_factory` build the audition project and start the stream directly,
with no preview trim. `PREVIEW_PEAK_CEILING_DB` and the peak helper live
behind `#[cfg(test)]` as the assertion the tests use, and
`render_prepared_audition` is test-only, so neither the debug nor the release
application pre-renders a preview.

The preview still pays for the instrument's per-patch probe the first time a
patch is heard in a process; after that the trim is cached. The probe is a
3.5-second render at 48 kHz, so it costs milliseconds in an optimised build
and nothing on a second click.

A SoundFont or hosted plugin instrument sits outside the synth's contract: its
own sample level applies and the preview does not trim it. The bank's own
patches are all OpenSynth patches and stay under the ceiling.

## Related

- [Synth character](synth-character.md)
- [Filter character](filter-character.md)
- [FX laws](fx-laws.md)
- [Factory presets](factory-presets.md)
