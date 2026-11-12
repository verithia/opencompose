# Audio clip time-stretch

Scope: the clip stretch surface (DSL, ratio resolution, derived-media cache,
track invalidation, load-time budget) and the two stretch algorithms: WSOLA for
`mode: percussive` and a phase vocoder for `mode: musical`.
See `docs/audio-clip.md` for decode, resampling, placement and the streaming
lane itself.

Both stretch modes are implemented: percussive WSOLA and the `mode: musical`
phase vocoder. Both are baked into the derived cache and the mix path still
reads one buffer 1:1.

## 1. DSL surface

```ocp
@project "Stretch"
@track tape | instrument: audio/clip
@audio_clip intro [1:1 - 5:1] | track: tape
  file: "media/intro.flac"
  stretch: 1.25        // output duration / media duration; 1.0 = untouched
  mode: percussive     // off | percussive | musical | varispeed
  tempo_follow: true   // derive the ratio from the range and the media length
```

| Property | Meaning | Default |
|---|---|---|
| `stretch:` | time-stretch factor: output duration = media duration × `stretch` | `1.0` |
| `mode:` | algorithm; `off` disables processing even when a ratio is set | `musical` |
| `tempo_follow:` | ignore `stretch:` and derive it at load (see below) | `false` |

Semantics:

- **Varispeed** (`mode: varispeed`) links playback speed and pitch. At
  `stretch: 2`, the sample plays at half speed, one octave lower. At
  `stretch: 0.5`, it plays twice as fast, one octave higher. Varispeed
  uses band-limited resampling and requires zero independent pitch and
  formant shift, formant preservation off, and an empty warp list. In the
  Clip Playback panel choose **Varispeed (tape)**. Alt-drag the right edge to
  change placement length and rate together, or set a speed in the panel
  and keep the existing placement. Fit to clip also works with varispeed.

- **1.0 is a bit-exact bypass.** The decoded/resampled buffer is used as-is;
  no variant is baked, cached or allocated. `mode: off` forces the same bypass
  for any ratio, which keeps an A/B path.
- **`tempo_follow`** fits the whole decoded media to the clip range:
  `ratio = range frames / media frames` at the project rate. A tempo or
  tempo-map change moves the range in frames, so the ratio is recomputed.
  It takes precedence over an authored `stretch:`. Applying **Fit source to clip
  length** in the GUI, or explicitly sending `tempo_follow: true` through
  `audio_clip_set_playback`, clears the existing source offset and loop so the
  full fitted tail can play. That edit refuses an explicit duration ratio or
  processing mode `off`. Other playback edits that omit `tempo_follow` preserve
  authored offsets and loops; opening an older project does not rewrite them.
- **Offset is output-time** when stretch is active: the whole media is
  stretched first, then `offset:` trims the stretched buffer. This keeps the
  cache key independent of `offset` (slip editing does not invalidate
  variants) at the cost of offset seconds meaning stretched seconds.
- The clip's musical range still decides placement and project length. Longer
  stretched media is trimmed by the range; shorter media holds silence after
  it ends, exactly as before.
- Ratios outside `0.25..=4.0` are refused with the clip name at validate and
  render time.
- **`pitch:` composes with the ratio.** A transposition reads the media at
  `2^(semitones/12)` times the project rate and multiplies the stretch ratio by
  the same factor, so the clip keeps `stretch` × media duration. The composed
  ratio — not the written `stretch` — is what the range check above sees. The
  range is `-24..=+24` semitones; see `docs/audio-clip.md` section 3 for the
  formant note.
- **`mode: off` with a non-zero pitch is refused.** There is no pitch-only
  bypass: without a stretch the transposition would shorten the clip.

## 2. Derived-media cache

The existing media key (`path`, `mtime`, `size`) is untouched. A stretch
variant has its own key:

```
StretchKey = MediaKey                     # source file version
           + target_rate                  # project rate
           + ratio_bits                   # f64 bits, so any change misses
           + mode                         # percussive | musical | varispeed
           + pitch_rate                   # playback-speed resample rate, 0 = no pitch
           + ALGO_VERSION                 # bumped by any DSP change
```

- `pitch_rate` is part of the key even though the ratio already carries the
  pitch, because two plans can compose to the same ratio from different pitches
  (`stretch 2 × pitch 0` and `stretch 1 × pitch +12` both bake at 2.0) while
  their inputs differ. A variant without pitch keeps the previous on-disk name,
  so an existing cache is not invalidated by the field appearing.
- The on-disk name is `md5(stamp).ocd` in the media cache directory; the file
  is an `OCMD` v1 entry: 26-byte header (magic, version, flags, access stamp,
  rate, frames) followed by interleaved f32 samples.
- `ALGO_VERSION` invalidates old variants when the DSP changes. Version 3
  includes local formant correction, and the key distinguishes the live and
  export resampling rungs.

## 3. Load-time byte budget

The streaming loader resolves stretch plans and counts each unique processed
variant once. When the projected PCM fits the 256 MiB derived memory budget,
it keeps the resident buffers and shares repeated placements. When it
exceeds the budget and the configured disk limit can accommodate it, the
loader bakes to seekable PCM files. Completed files are published atomically
and can be reused without rebaking on the next open. The render worker reads the requested
blocks by frame offset, including after seeks and across loop boundaries.
The audio device callback still consumes the worker's ring buffer.

An individual variant over the memory budget is baked in overlapping
windows with bounded scratch space. Authored warp boundaries retain their
frame positions. Stretch and formant analysis restart at window boundaries
and can sound different from a single uninterrupted bake. Offline clip
mixing uses the same bounded bake for an individual variant over budget.
If disk storage is disabled or too small, the load reports the budget error.

The source media cache also spills decoded and resampled PCM above its
per-source resident threshold. Decoding writes packets, waveform peaks are
built from blocks, and rate conversion reads only the source window needed by
each output block. Offline export still allocates its destination track
buffers. Named PCM cache entries follow their disk eviction policy. Any
temporary spool that could not be published is released with its last reader.

## 4. Track invalidation

Dirty flags alone cannot catch a forgotten flag, which would serve a track
buffer containing the old stretch. Each track therefore stores an FNV-1a fingerprint of its audio clips: name, file, range, offset,
gain, fades, stretch, mode, tempo-follow, and the clock-derived start/length.
A mismatch marks the track dirty. A dirty track starts from an empty buffer,
so a clip-only track never accumulates its previous render.

The fingerprint hashes clip parameters, not file content. A media file that
changes while a project is open keeps the loaded decode (documented in
`docs/audio-clip.md`); the derived key does pick up the new `mtime` on the
next load.

## 5. Percussive WSOLA (`mode: percussive`)

- 1920-sample Hann analysis window, 960 hop (50 % overlap, windows sum to 1),
  ±480-sample similarity search, coarse on a 4× decimated copy then refined to
  one sample.
- **Transient anchoring.** The input is cut at detected attacks. Each segment
  starts with a flat copy of its attack at `round(ratio × onset)` and then
  continues with WSOLA inside the segment. Without anchoring, 50 % overlap-add
  spreads an impulse over the neighbouring windows: a 2:1 click train produced
  half-amplitude copies up to 10 ms early. Segment boundaries get a 32-sample
  fade so the splice cannot click.
- The similarity search correlates the mono sum so both channels move together;
  a hard-panned signal keeps its image. Hann windows, zero-padded source outside
  the segment, and a normalised tail on the final segment. Media shorter than
  40 ms uses a proportionally shorter window and hop.
- Everything runs offline or at load. `ClipLane::mix_into` still only copies
  from the baked buffer: no allocation, no lock, no decode in the audio
  callback.

## 6. Musical phase vocoder

`mode: musical` is a block phase vocoder with identity phase locking, a phase
reset at every detected onset, a local ratio between resets, and a short
one-to-one splice around each attack.

### 6.1 STFT and normalisation

- 2048-point FFT and a 512-sample synthesis hop (75 % overlap) at the project
  rate. The analysis hop is `512 / ratio` and lands on whole samples; the phase
  advance uses the hop that was actually taken, so rounding never accumulates.
- sqrt-Hann analysis and sqrt-Hann synthesis window. Their product is a periodic
  Hann, whose sum over the four overlapping frames is exactly 2 in the interior.
- **Normalisation:** overlap-add accumulates the window product per output
  sample and divides by that sum. The sum is 2 in the interior and smaller only
  where the frame grid begins or ends; dividing by the actual sum keeps those
  edges exact instead of fading them in. The inverse FFT is unnormalised, so
  each frame is also scaled by `1 / 2048`.
- At ratio 1.0 the analysis grid equals the synthesis grid and the accumulated
  phase equals the analysis phase, so the transform pair is transparent:
  measured reconstruction error is `1.2e-7` of full scale.

### 6.2 Phase advance and identity locking

Per bin and frame the deviation is `princarg(phi_m - phi_{m-1} - 2*pi*k*hop/N)`,
the instantaneous angular frequency is `2*pi*k/N + deviation/hop`, and the
synthesis phase advances by `omega * 512`. The complex spectrum is rebuilt from
the analysis magnitude and the accumulated phase, with conjugate symmetry and
real DC/Nyquist bins.

Locking is identity phase locking: peaks are 3-bin local maxima above -60 dB of
the frame peak, and every bin between two adjacent magnitude valleys takes its
region peak's synthesis phase plus its own analysis phase offset from that peak.
The peak decision uses the sum of the left and right magnitudes and is applied
to both channels, so a hard-panned source keeps its image.

### 6.3 Onset detector

- Spectral flux on 1024-point Hann frames at a 256 hop:
  `F(m) = sum(max(0, log(1+|X_m|) - log(1+|X_{m-1}|))) / (1 + sum(log(1+|X_m|)))`.
  The rise is normalised by the frame's own log energy, so the threshold does
  not depend on the clip level.
- An onset is `F(m) > 1.5 * median(F(m-10..m+10)) + 0.05`. The median tracks a
  busy background: steady noise sits at 0.15 and raises its own threshold to
  0.28, stationary tones at 0.0005-0.002 fall below the floor, and a broadband
  attack scores 0.9-1.0.
- Sample-level refinement takes the largest 64-sample energy rise inside the
  flagged frame and the last sample of that rise plateau, which lands on the
  attack sample for an impulse. Onsets closer than 256 samples (5.3 ms) merge.
- Frames whose window would run past the end are not tested, so an attack in
  the last 10 ms is weakly weighted and can be missed.

**Failure mode:** the floor makes the detector level-dependent. A soft or
gradual attack whose flux stays under 5 % of its frame's log energy, or any
attack inside a loud sustained bed, is not detected and keeps the vocoder's
smear. Magnitude flux also misses attacks that barely change the magnitude
spectrum, such as a bowed-string onset.

### 6.4 Reset, local ratio and the transient splice

Anchors pair each detected onset with the output frame nearest
`round(onset * ratio)`. Between two anchors the analysis position moves linearly,
so the local ratio is `source distance / output distance`, and the frame at an
anchor resets the synthesis phase to the analysis phase (locking then makes the
whole frame identical to the analysed one). Anchors sit on the synthesis grid,
so a reset is exact.

The reset alone cannot make an attack sample-accurate: a 2048-point window
smears an impulse over ±21 ms, and at a reset the attack is only as precise as
the ±256-sample frame grid. Each onset therefore also gets a one-to-one splice:
the original samples for `min(1024 * ratio + 512, half the gap to the
neighbouring onset)` either side of the attack are written over the vocoder
output at `round(onset * ratio)`, with a 128-sample fade at each end. That is
what makes onsets sample-accurate and what removes the pre-echo. The cost is
that up to `2 * (1024 * ratio + 512)` output samples around an attack (85 ms at
1.5x) play at 1.0x instead of the clip ratio.

Everything runs at bake time. `ClipLane::mix_into` still copies from the baked
buffer: no allocation, no lock, no decode and no FFT in the audio callback.

## 7. Measured numbers

48 kHz stereo, the golden-signal harness in
`crates/oc-engine/tests/suite/audio_clip_stretch.rs`; the phase-vocoder unit numbers
come from `crates/oc-engine/src/phase_vocoder.rs`.

| Case | Target | Measured |
|---|---|---|
| 1.0× bypass (768,000 samples, click train) | bit-identical | 0 samples differ |
| 10 clicks × 100 ms, 2:1, percussive | every onset within 2 ms | 10 onsets, max error 0 samples (0.000 ms), 0 extras above −40 dB |
| 10 clicks × 100 ms, 1.5:1, percussive | every onset within 2 ms | 10 onsets, max error 0 samples (0.000 ms), 0 extras above −40 dB |
| 10 clicks × 100 ms, 2:1, musical | every onset within 2 ms | 10 onsets, every error 0 samples; loudest sample outside ±2 ms −51.0 dB |
| 1 kHz, 1 s, 5:4, musical | within 1 cent | 1000.0000 Hz, 0.000 cents |
| 1 kHz, 1 s, 3:4, musical | within 1 cent | 1000.0000 Hz, −0.000 cents |
| Level, 5:4 tone, musical vs bypass | — | −0.00 dB |
| 220 + 440 + 660 Hz chord, 0.75 and 1.25, musical | partials within 2 cents, no beating | worst partial 0.000 cents; level spread 1.42 dB (DFT scalloping); 100 ms envelope ripple 0.12 / 0.34 dB |
| Dense triad 261/330/392 Hz, 0.75, musical | — | partial level spread 2.2 dB, of which 1.4 dB is scalloping |
| Pre-echo, impulse at 1.0 s, 1.5:1, musical | ≤ −30 dB before target − 5 ms | −75.6 dB, attack at sample 72000 exactly; the vocoder without the splice measures −31.2 dB |
| Splice seam, sustained chord, first 100 ms, musical | — | 1.6 / 3.9 / 6.2 dB envelope spread at 0.75 / 1.25 / 2.0 (a 2.7 ms dip) |
| Overlap-add reconstruction at 1.0 | — | max error 1.2e-7 |
| Output length | `round(input × ratio)` | exact |
| Determinism / cache hit vs fresh bake | bit-identical | bit-identical |
| `musical` vs `percussive` | different algorithms | renders differ (asserted) |
| 440 Hz, 2 s, `pitch: 12` | 880 Hz within 1 cent, 2 s | 880.0000 Hz (+0.0000 cents), 96,000 frames |
| 440 Hz, 2 s, `pitch: -12` | 220 Hz within 1 cent, 2 s | 220.0000 Hz (-0.0000 cents), 96,000 frames |
| 440 Hz, 2 s, `pitch: 5` | 587.33 Hz within 1 cent, 2 s | 587.3149 Hz (-0.0432 cents), 96,000 frames |
| 440 Hz, 2 s, `stretch: 1.25` + `pitch: 5` | 587.33 Hz, 2.5 s | 587.3617 Hz (+0.0949 cents), 120,000 frames |
| `pitch: 0` against the parent, three stretch settings | bit-identical | 0 samples differ |
| Boundary, 5 ms fades, `pitch: 12` | no step, no spill | 5 ms either side are zero; edge samples are zero |

The theoretical pre-echo of a 2048-point window is 21 ms. Measured, the vocoder
alone puts the ring 31 dB below the attack peak before it, and the splice pushes
that to −75.6 dB: the residual is at the level of the splice fade, not of the
window.

## 8. Failure modes

- **Ratio out of range** — validation and render name the clip and the bound.
  The reported ratio is `stretch × pitch`, so a large stretch plus a large pitch
  is refused even when each is written inside its own bound.
- **Pitch out of range** — validation and render refuse beyond ±24 semitones
  and name the clip; a non-zero pitch on `mode: off` is refused rather than
  shortening the clip.
- **Formant movement** — a transposition moves the formants by default.
  `preserve_formants: true` applies an approximate inverse spectral-envelope
  shift to the processed clip. `formant:` adds an independent shift in
  semitones. The combined correction must fit within ±12 semitones.
- **Live and render rungs differ** — the pitch step resamples at the rung each
  path chose (`live_resample` for the streaming lane, the export rung for a
  render), so the two can differ by that kernel. Pin both to the same rung and
  they bake one variant; the streaming test does exactly that.
- **Project over the derived budget** — the live loader spools the processed
  variants when configured disk capacity permits it.
- **Variant over the budget** — the live and offline mixers bake in bounded
  windows. Loading still needs the original decoded media.
- **DSP change without a version bump** — the one remaining manual step: bump
  `ALGO_VERSION` in `crates/oc-engine/src/stretch.rs`. A variant baked under
  an earlier version has a different cache key.
- **Media version change** — a new `MediaKey` means a new variant; the open
  project's pinned lane keeps the old decode until reload, like decoded media.
- **Stale track buffer** — the clip fingerprint re-renders the track.
- **Trimming or splitting a tempo-follow clip** — the command freezes its
  existing fit as an explicit ratio and retains the source mapping.
- **Offset on a stretched clip** is output-time, not source-time.
- **Undetected onsets (`musical`)** — a soft, gradual or masked attack keeps the
  vocoder's smear: up to 21 ms of pre-echo, and in output time up to
  `21 ms × ratio`.
- **Transient splice cost (`musical`)** — around each detected attack the output
  is the original material at 1.0x for up to 85 ms at 1.5x. Where the copy
  fades back into the vocoder, out-of-phase content cancels: a sustained chord
  dips by 3.9 dB at 1.25x and 6.2 dB at 2x for 2.7 ms, measured on the start
  splice (an attack elsewhere masks the seam). On a dense onset stream the
  splice is clamped to half the gap to its neighbours.
- **Dense partials (`musical`)** — 2048 points give 23.4 Hz bins, so partials
  closer than about three bins lock into one region and lose level: a 68 Hz
  spaced triad lost about 0.8 dB on its inner partial. A longer or
  multi-resolution analysis is the fix and is not implemented.
- **Phase-unwrapping limit (`musical`)** — the deviation is only unambiguous
  within half a bin (11.7 Hz). Content whose instantaneous frequency moves
  further per hop, such as noise or fast sweeps, is resynthesised at the wrong
  frequency; this is the standard phase-vocoder limitation.
- **Boundary attacks (`musical`)** — an attack in the last 10 ms of the media is
  weakly weighted or missed, because the detector's frames would run past the
  end.
- **Transient-detector false positives (percussive)** — material that dips below
  −20 dBFS of the peak creates extra segment boundaries; each splice is a
  32-sample fade.
- **Sustained tonal material at ≥1.5× (percussive)** — WSOLA repeats waveform
  segments ("machine gun"); `mode: musical` is the fix.
- **Tail** — the last `hop × (1 − 1/ratio)` frames fade where the source window
  runs out (10 ms at 2×, 4 ms at 1.25× for percussive; the vocoder's tail is
  zero-padded and normalised instead).

## 9. Verification

```sh
cargo test -p oc-dsl --test all semantics::audio_clip::
cargo test -p oc-engine --lib stretch
cargo test -p oc-engine --lib phase_vocoder
cargo test -p oc-engine --lib derived_cache
cargo test -p oc-engine --test all clips::audio_clip_stretch::
cargo test -p oc-engine --test all clips::audio_clip_stretch::pitch
cargo test -p oc-engine --test all clips::
cargo test -p oc-engine --test all clip_lane_alloc::
```

## 10. Warp markers and stretch gestures

A warp marker maps source seconds to processed seconds. The first segment
starts at the implicit origin. Each segment uses the clip's stretch algorithm
and the processed length set by its two anchors. After the last anchor, the
clip uses its explicit stretch ratio. The derived cache distinguishes every
anchor set and formant correction.

Drag the selected clip’s ↔ handle or Alt/Option-drag its right edge to scale the mapping and change
its end in one edit. A regular trim leaves the mapping in place. Use the
Speed and pitch editor to type, add or remove warp anchors. Drag visible
anchors to move their target times. Use **Snap** in a marker row to look for
a source attack within 75 ms of the entered source time. The waveform maps
its source peaks to the current target times.

Agents can call `audio_clip_transients` with the clip index and an optional
source window of up to 30 seconds. It returns the source duration, analysis
resolution and candidate attack times ranked by strength. Candidate times
are before clip offset and processing and can be used as warp source times.

Set `bypass_processing: true` or use **Bypass processing** to play the
original media in the same placement. The clip retains its stretch, pitch,
formant and warp settings for when processing is enabled again.

## 11. What is still missing

- A multi-resolution or HPSS analysis so dense chords and attacks share one
  path; the fixed 2048-point window is the current quality ceiling.
- A complex-domain (phase-deviation) onset detector for soft and bowed onsets.
- De-click and short-gap spectral repair.
- Progressive cold start for long processed media. The render worker prepares the spool before playback.
- Any form of real-time stretch: the audio callback must stay allocation-free.
