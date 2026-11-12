# OpenSynth factory presets

OpenSynth ships with the four original patches (`Init`, `Neon Bass`,
`Glass Pad`, `Pulse Pluck`) plus the 62 category presets below, so the browser
lists 66 patches. Every preset is a set of overrides on `Init`, so loading one
starts from the init voice and changes only the listed controls.

Open the patch editor's browser, pick a preset and press **LOAD**. The load
replaces the whole patch — parameters, effect chain and the `patch:` provenance
label — in one undoable step, and resets every key the preset does not declare.
The browser lists the bank under its categories and a **User library** section
below it; both use the same search, `▶` audition and load affordances. The
*figure* column is the phrase each preset is written for; it is a starting
point, not a restriction. The browser's own audition always plays one six-note
figure, so an audition compares timbres rather than phrases.

| Preset | Category | What it sounds like | What it demonstrates | Figure |
|---|---|---|---|---|
| Deep Anchor | Bass | A sine sub-bass that anchors the lowest note without adding mud. | — | C2 / C2 / Eb2 riff |
| Rubber Bass | Bass | A round saw bass with a soft filter bounce for warm house and funk lines. | — | C2 / C2 / Eb2 riff |
| Acid Ladder | Bass | A resonant ladder bass made for sliding, squelchy sixteenth-note lines. | Ladder resonance and oversampling | C2 / C2 / Eb2 riff |
| Growl Engine | Bass | A dirty, wobbling bass with a slow filter lurch for heavy electronic grooves. | LFO on the filter | C2 / C2 / Eb2 riff |
| Short Circuit | Bass | A tight, plucky square bass that leaves room for kick and sub. | — | C2 / C2 / Eb2 riff |
| Round Upright | Bass | A soft triangle bass with a gentle decay that sits like an upright. | — | C2 / C2 / Eb2 riff |
| Sub Pressure | Bass | A pressurised sub with a clipped noise click so it still reads on small speakers. | Sub and noise layered under one filter | C2 / C2 / Eb2 riff |
| Broad Shoulder | Bass | A wide bass that spreads osc B across four unison voices an octave down. | Per-oscillator unison on osc B | C2 / C2 / Eb2 riff |
| Fold Growl | Bass | A folded saw through a dirty ladder whose drive and resonance breathe with an LFO. | Fold warp into a dirty ladder | C2 / C2 / Eb2 riff |
| Thumb Pluck | Bass | A short, velocity-bright pluck bass that leaves the sustain to the sub. | Velocity opens the filter | C2 / C2 / Eb2 riff |
| Knife Solo | Lead | A bright five-voice saw lead for cutting through a busy mix. | — | four-note phrase around C5 |
| Square Beacon | Lead | A wide pulse lead with a slow filter shimmer for sustained melodies. | PWM with a tempo-synced LFO | four-note phrase around C5 |
| Sync Sweep | Lead | A hard-synced lead with a metallic edge for aggressive hooks. | Hard sync plus a second filter | four-note phrase around C5 |
| Octave Runner | Lead | A saw lead stacked an octave up for fast, soaring runs. | — | four-note phrase around C5 |
| Wire Whistle | Lead | A thin triangle lead with gentle vibrato that sings above the mix. | — | four-note phrase around C5 |
| Step Needle | Lead | A rhythmic pulse lead whose filter steps through a sixteenth-note pattern. | Step sequencer drives the cutoff | four-note phrase around C5 |
| Mono Vector | Lead | A mono wavetable lead whose frame position is swept by the third envelope. | Envelope 3 drives the wavetable position | four-note phrase around C5 |
| Filter Cry | Lead | Two band-pass filters in parallel give this lead a vowel-like cry. | Two parallel band-passes form a vowel | four-note phrase around C5 |
| Ladder Solo | Lead | A driven ladder lead with a biting square layer on top. | Four-pole ladder with drive | four-note phrase around C5 |
| Echo Lead | Lead | A chorus-and-delay lead whose repeats sit behind the dry square. | FX rack: chorus into delay | four-note phrase around C5 |
| Slow Horizon | Pad | A wide, slow saw pad that opens like a distant horizon. | — | sustained three-note chord |
| Velvet Choir | Pad | A soft, breathy pad with a smooth ensemble swell for chord beds. | — | sustained three-note chord |
| Frozen Glass | Pad | A bright, glassy pad with a slow shimmer for cinematic textures. | — | sustained three-note chord |
| Deep Drift | Pad | A dark, low pad that drifts under the mix with a long release. | — | sustained three-note chord |
| Breathing Air | Pad | A noise-based airy pad that breathes with a slow band-pass sweep. | Noise through a band-pass sweep | sustained three-note chord |
| Hollow Choir | Pad | A hollow square pad with a gentle pulse movement for wide chords. | — | sustained three-note chord |
| Cathedral Sweep | Pad | A slow ladder pad that opens under a long reverb tail. | FX rack: long reverb | sustained three-note chord |
| Frame Bloom | Pad | A wavetable pad whose two oscillators drift across the frame bank at different rates. | LFO sweeps the wavetable position | sustained three-note chord |
| Vowel Drift | Pad | Parallel band-pass filters sweep against each other for a breathing vowel bed. | SVF formants on two parallel bands | sustained three-note chord |
| Night Current | Pad | A dark current of detuned saws under a slow ladder swell. | Sub-heavy ladder pad | sustained three-note chord |
| Wash Chorus | Pad | A noisy triangle wash widened by chorus and a long delay. | FX rack: chorus plus delay | sustained three-note chord |
| Drift Stack | Pad | Each oscillator carries its own unison spread, so the chord widens in three layers. | Per-oscillator unison on all three | sustained three-note chord |
| Short Wire | Pluck | A crisp square pluck for short, percussive melodic hooks. | — | eighth-note arpeggio |
| Glass Drop | Pluck | A bright bell-like pluck that rings with a clean, glassy tail. | — | eighth-note arpeggio |
| Harp Thread | Pluck | A warm triangle pluck with a soft tail for arpeggios and counterlines. | — | eighth-note arpeggio |
| Koto Spark | Pluck | A bright, picked pluck with a sharp attack for fast arpeggios. | — | eighth-note arpeggio |
| Ring Spark | Pluck | A ring-modulated bell pluck with a short metallic body. | Ring modulation | eighth-note arpeggio |
| Pick Wood | Pluck | A quantised triangle pluck with a noise pick and velocity-bright filter. | Quantize warp plus velocity | eighth-note arpeggio |
| Pulse Drop | Pluck | A pulse-width pluck whose narrow duty cycle bites through dense mixes. | PWM warp on a square | eighth-note arpeggio |
| Echo Pluck | Pluck | A clean pluck fed into a short delay and a small room. | FX rack: delay into reverb | eighth-note arpeggio |
| Tine Strike | Pluck | A struck tine through a resonant band-pass, bright when played hard. | Band-pass pluck | eighth-note arpeggio |
| Electric Tine | Keys | A soft electric-piano tine with a bell-like attack for chords. | — | four-chord comping |
| Soft Keys | Keys | A mellow triangle-and-saw keyboard for warm chord comping. | — | four-chord comping |
| Toy Piano | Keys | A small, bright square keyboard with a short, playful decay. | — | four-chord comping |
| Glass Bell | Keys | A ringing bell keyboard with a long, glassy release. | — | four-chord comping |
| Dust Tine | Keys | A saturated electric piano that growls when the chord is played hard. | Tape saturation on a tine | four-chord comping |
| Clav Bite | Keys | A band-passed square clav with a fast decay for funk chord stabs. | PWM into a band-pass | four-chord comping |
| Muted Grand | Keys | A damped grand with a hammer edge: short decay at the top, long at the bottom, quick key-off. | Hammer layer and register damping | four-chord comping |
| Phase Keys | Keys | A sine-and-triangle keyboard kept in motion by a slow phaser. | FX rack: phaser | four-chord comping |
| Swell Brass | Brass | A slow-blooming brass section with a soft filter swell. | — | stab and held chord |
| Stab Brass | Brass | A punchy brass stab with a bright attack for horn hits. | — | stab and held chord |
| Muted Brass | Brass | A dark, muted brass with a short decay for tight riffs. | — | stab and held chord |
| Horn Line | Brass | A tenor horn line built from a saw, a fifth and an octave. | Saw, fifth and octave stack | stab and held chord |
| Big Stab | Brass | A stacked ladder brass hit with a hard filter snap on the attack. | Ladder stab with a filter snap | stab and held chord |
| Cinema Swell | Brass | A slow brass swell that blooms into a large hall. | FX rack: reverb on a swell | stab and held chord |
| Riser Tension | Fx | A slow noise-and-saw riser that builds tension into a drop. | Filter-envelope riser | two-note swell |
| Metal Drone | Fx | A ringing metallic drone with a slow moving filter for atmospheres. | Ring modulation | two-note swell |
| Tape Storm | Fx | A stormy, saturated texture with a wandering filter and long tail. | — | two-note swell |
| Bloom Riser | Fx | A noise-and-saw riser that opens across several seconds before a drop. | Rising filter envelope | two-note swell |
| Chaos Field | Fx | Two chaos LFOs pull the filter, width and frame position in different directions. | Chaos LFOs on filter, width and frame | two-note swell |
| Gate Grid | Fx | A stepped sixteen-note gate that chops the square into a rhythmic grid. | Step sequencer gates the oscillator | two-note swell |
| Deep Space | Fx | A wavetable drone carried by a chorus, a long delay and a huge reverb. | FX rack: chorus, delay and reverb | two-note swell |

## Category notes

- **Bass** (10): monophonic and centred, with short filter and amplitude
  releases so the low end stays tight under a kick. The family covers a sub
  anchor, a wide per-oscillator unison bass, a folded growl, and two pluck
  basses.
- **Lead** (10): bright single-line voices, mostly monophonic with glide. Three
  of them show one engine area each — an envelope driving the wavetable
  position, two parallel band-passes for a vowel, and a driven ladder.
- **Pads** (12): eight-voice polyphony with slow attacks, wide detuned unison
  and long releases. The family exercises wavetable sweeps, SVF formants, a
  sub-heavy ladder bed and three different FX racks.
- **Pluck** (9): sharp attacks and short decays for riffs, hooks and arpeggios,
  including a ring-modulated bell, a PWM pluck and one delay/reverb pluck.
- **Keys** (8): polyphonic chord patches with a decaying body, from clean tines
  and bells to a saturated tine, a band-pass clav and a phaser keyboard.
- **Brass** (6): mid-forward swells and stabs, from a three-oscillator horn line
  to a reverb-backed cinema swell.
- **FX** (7): moving textures rather than played lines — risers, drones, a chaos
  field, a step-sequencer gate and a three-module ambient chain.

## Level and timbre bounds

The bank is measured headless on every change, so a preset cannot silently
arrive quiet, broken or hot. Every preset renders the audition figure (six
eighths at 140 BPM) in its own register — bass two octaves down — plus a one
second release tail, and the render is checked against:

- **Peak**: at most −1 dBFS at the patch output, with no sample at full scale.
- **Level**: RMS inside the category window; the whole bank stays within a
  10 dB RMS span.
- **Audition loudness**: the bank probe matches each candidate to the bank
  reference of −17.2 LUFS, and the 62 patches span 14.9 LU before matching and
  under 0.4 LU after it, so the table compares timbres instead of levels. The
  reference is the median integrated loudness measured through the same
  six-note audition figure with the renderer's BS.1770-4 meter. The live
  preview does not carry that match: it plays the patch at the level the
  instrument produces, and every stage that can raise the voice path carries a
  normalisation so the bank stays below −6 dBFS without the browser rendering
  and measuring the figure first. The loudest patch is −6.21 dBFS; see
  `docs/synth-levels.md`.
- **Crest factor** and **spectral centroid** inside a category window, so a
  patch that collapses (all sustain, no attack) or turns into noise fails.
- **DC offset** below 0.5 % of full scale, and no NaN/Inf samples.
- **Silence**: every preset must peak above −24 dBFS.

`scripts/check-factory-presets.py` implements the probe and the bounds; run it
with `--ocp target/debug/ocp` (or the `ocp` binary of the current build) after
changing the bank. The measured table for the current bank is in the task note,
and the frontend tests check the data side: unique names, per-category counts,
parameter keys the factory table accepts, complete envelopes and no placeholder
text.

Two engine properties shape the sound design here. `osc.<x>.pulse_width` away
from `0.5` and the narrow-pulse frames of the default wavetable bank (positions
above `0.5`) carry a DC component, because the readout preserves the frame mean;
the bank therefore takes its narrow pulses from the PWM warp, which is
DC-free, and parks wavetable positions at or below `0.5`.
