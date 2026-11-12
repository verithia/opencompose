# Comping: take alignment and take lanes

Status: loop recording, take lanes, the comp gesture, take mute and session
solo are in. `ocp clip align`, take polarity and the comp crossfade are the
offline half. The GUI half draws one row per take and writes the comp as splits
and mute flags in the DSL.

## 1. What it does

Several takes of one performance are placed as `@audio_clip` blocks. One clip is
the reference; every other take is analysed against it. The result is written
back into the same clip fields the renderer already consumes:

```ocp
@project "Comp"
@track take_b | instrument: audio/clip
@audio_clip take_b [1:1 - 5:1] | track: take_b
  file: "media/take_b.wav"
  offset: 0.0295s     // head trim, output time
  stretch: 0.985222   // output duration / media duration
```

No new DSL field, no new render path and no second stretch implementation: the
ratio goes through `docs/audio-clip-stretch.md`, and `offset` is applied after
the bake exactly as before.

## 2. Analysis

All of this runs offline (CLI or worker thread), on the project-rate media from
the media cache. Only the part a clip can play at 1.0x is analysed:
`min(media frames, clip range frames)` from the start of the media.

**Onsets.** A 10 ms energy window with a 2.5 ms hop produces a log-energy rise
per frame. The threshold is `1.5 ×` the local median with a `0.05` floor and a
local-maximum test. Each candidate is refined to a sample with a 16-sample block
energy rise followed by the first sample above 5 % of the take's peak. Attacks
closer than 15 ms merge. A clip whose first frame already carries sound gets an
onset at sample 0. The detector returns ascending sample indices; the analysis
converts them to seconds, so every number below is rate-independent.

**Coarse map.** The model is

```
take_time = a × reference_time + b
```

A plain DTW over absolute onset times cannot estimate `a` when the tempo
difference is large: nearest-in-time pairing biases the slope towards 1 (a 5x
take came out at 0.9 instead of 0.2 in the first implementation). The map is
therefore initialised by RANSAC over pairs of onsets that are at least 10 % of
the span apart. Each hypothesis is scored by a greedy one-to-one monotonic
match within 1 % of the span (5–50 ms); the best hypothesis is kept. Sampling is
deterministic (fixed-seed LCG), so the same inputs always give the same map.

**Monotonic correspondence.** The take is warped onto the reference timeline
with the coarse map, then DTW runs on the warped onset times. A diagonal match
costs `|reference − take|` capped at 200 ms; leaving an onset unmatched on
either side costs 50 ms. The backtracked path is the monotonic correspondence:
extra and missing attacks are skipped rather than paired.

**Robust fit.** The path gives paired onset times. Theil–Sen (median of pairwise
slopes, pairs at least 50 ms apart) and a median intercept give a fit that
ignores outliers; pairs within 5 ms are inliers, and least squares refits those.
Warping, DTW and the fit repeat twice, so the final map is fitted in its own
warped timeline. One onset gives a shift-only map (`a = 1`); zero onsets is an
error.

**Snapping.** A slope within `1e-6` of 1 is set to 1, and a trim below half a
sample is set to 0. That is what makes an already-aligned take a bit-exact no-op
rather than a one-sample slide.

## 3. Applying the correction

For `take_time = a × reference_time + b`:

```
stretch ratio s = 1 / a
head trim       d = b / a + (take clip start − reference clip start)
```

`d` is in output time, because the existing stretch bakes first and trims the
stretched buffer afterwards. Substituting the map shows the result is exactly
the reference timeline:

```
output_time(media) = s × media − d
                   = (a × reference + b) / a − b/a − (start difference)
                   = reference − (start difference)
```

- `d >= 0` is written to `offset:`. `s` is written to `stretch:`.
- `s == 1` is a bypass: no variant is baked; only `offset:` changes. This is
  consistent with the existing derived-cache design, where `offset` is
  deliberately not part of the key because slip editing must not invalidate a
  bake.
- `s != 1` is a new `StretchKey` (`media + rate + ratio bits + mode +
  ALGO_VERSION`) and therefore a new bake. Re-running an alignment that
  produces the same ratio hits the existing key; a different ratio cannot be
  served stale.
- `write_into` refuses a non-identity correction when `tempo_follow: true`
  (the derived ratio would override the alignment) and refuses a ratio
  correction when `mode: off` (it would be silently ignored). Pure shifts are
  allowed with either, because they do not use the stretch path.
- Re-aligning overwrites `offset` and `stretch`; analysis always starts from the
  raw media, never from a previous correction.

## 4. Take polarity

After the affine fit, `analyze` maps every reference frame through the fitted
map and samples the take there (linear interpolation, both channels). The
normalised correlation is

```
r = sum(a b) / sqrt(sum(a^2) sum(b^2))
```

over the aligned overlap. It is reported for every take and does not depend on
gain: a gain-scaled copy measures near +1, a polarity-inverted copy near -1, and
unrelated material near 0. A take is out of phase when `r < -0.25`
(`align::POLARITY_THRESHOLD`). The threshold leaves room for noise and small
performance differences; the value is a polarity test, not a similarity score.

`invert: true` on an `@audio_clip` reverses the clip at mix time. Analysis always
reads raw media, so re-running an alignment measures the same polarity and never
compounds the flag.

`ocp clip align --invert` writes the flag when the measurement is below the
threshold and clears it otherwise. Without `--invert`, an existing flag is left
alone. A large stretch, a different performance or a noisy take lowers the
magnitude; a value near zero is not evidence of inversion.

## 5. Comp crossfade

`crossfade: <seconds>` gives a clip a quarter-wave equal-power fade at its head
and tail. With `N` the fade length in frames, the gain at offset `j` from the
head is

```
g_head(j) = sin(pi/2 * (j + 0.5) / N)
```

and at distance `k` from the last audible frame (`k = frames - 1 - frame`) is

```
g_tail(k) = sin(pi/2 * (k + 0.5) / N)
```

When two clips on one track overlap by exactly `N` frames and both carry
`crossfade:`, the outgoing tail is `cos(theta)` and the incoming head is
`sin(theta)` for the same `theta = pi/2 * (j + 0.5) / N`, so the gains squared
sum to one. For uncorrelated material the overlap power moves monotonically
from the outgoing clip's level to the incoming clip's level; equal levels stay
flat instead of adding two full-level clips. The law assumes the two sides are
not the same waveform in phase; two copies of one take still sum coherently, so
a comp seam should join different material rather than two identical clips.

The field is applied per clip and does not inspect neighbours: set it to the
overlap length. A clip with the field but no overlapping neighbour fades in and
out at its own ends. It composes with `gain`, `fade_in` and `fade_out`; when
`2N` exceeds the clip's audible length the head and tail envelopes overlap, so
use a shorter fade. The envelope is evaluated from integer frame indices, with
no allocation and no history in the mix path. When the field is absent the mixer
keeps the old summing behaviour bit for bit.

## 6. CLI

```sh
ocp clip align <PROJECT> <REFERENCE> <TAKE>...
  [--sample-rate HZ] [--dry-run] [--invert] [--output FILE]
```

`REFERENCE` and `TAKE` are `@audio_clip` names in `PROJECT`. The command
analyses every take before writing anything, so a failed take leaves the file
untouched. It writes the formatted project back to `PROJECT` unless `--output`
names another file; `--dry-run` prints the report and leaves the project alone.

```sh
ocp clip align song.ocp lead-take take-a take-b
ocp clip align song.ocp lead-take take-a --output aligned.ocp
ocp clip align song.ocp lead-take take-a --dry-run
```

Per take it prints matched onsets, tempo ratio, stretch ratio, head trim, the
largest inlier residual and the aligned correlation. `--invert` writes
`invert:` from that correlation; without it the flag is not touched. A
correction beyond the usable tempo band adds a warning on stderr. Analysis
defaults to 48 kHz; the written seconds and ratios do not depend on the render
rate.

## 7. Limits

- **Hard ratio range.** A correction needing a ratio outside `0.25..=4.0` fails
  with the take name and the required ratio. Nothing is clamped.
- **Usable tempo band.** We consider ±10 % tempo correction usable
  (`ratio 0.909..=1.1`). The CLI still writes the full 0.25..=4.0 range and
  warns outside ±10 %; past that the transient splice and phase locking of the
  stretch algorithms become audible as timbre and attack damage on most
  material. Re-record or split the take instead.
- **Pure shifts** can only trim the head. A take that starts before the
  reference would need a negative offset; placement is a bar:beat:subdivision
  grid that cannot express a millisecond delay, so the command fails with the
  required amount. A trim that consumes the media or the clip range also fails.
- **One onset** gives a shift only; tempo needs at least two matched onsets.
- **Affine only.** The correction is one offset plus one constant rate. Slow
  drift that is not linear is reported as a residual, not interpolated. A
  piecewise warp would need several baked segments and seams between them —
  another timing path, which this pass deliberately does not add. Split the
  clip at the drift point and align the pieces.
- **Detection.** Soft, gradual or masked attacks may be missed; a quiet intro
  relative to a later peak can be under the 5 % refinement floor; attacks
  closer than 15 ms merge; a reference with no detected attack cannot align.
- **Wrong notes.** Pitch does not affect onsets, so a wrong pitch is invisible
  to the fit. Extra or missing attacks are outliers for RANSAC and Theil–Sen and
  are not forced into the map; the matched count and residual show how much of
  the take actually agreed. A take whose onsets do not share one affine map gets
  a poor fit and a large residual rather than a forced warp.
- **Clip starts** are part of the correction: a take placed later than the
  reference needs that much more head trim.
- **Raw media.** The reference's own `offset`/`stretch` is ignored; align raw
  takes against a raw reference.

## 8. Measured numbers

48 kHz stereo synthetic takes; onsets detected after alignment and compared
with the reference (2 ms tolerance, last onset 3 ms).

| Case | Result |
|---|---|
| Reference shifted by +30 ms | 12/12 onsets, max error 0 samples (0.000 ms) |
| Take stretched by 1.5 % | 12/12 onsets, max error 0 samples; last onset 0.000 ms |
| Shift and 1.5 % stretch together | 12/12 onsets, max error 0 samples |
| Wrong note (extra attack 35 ms late) | 12 inliers, tempo 1.000000, trim 25.0 ms, residual 0.000 ms |
| Aligning an already-aligned take | 0 samples differ (bit-identical render) |
| Inverted take 30 ms late | correlation -1.000; mixed correlation -1.000 raw, +1.000 with `invert: true` |
| 10 ms equal-power crossfade | peak -60.000..-60.000 dBFS across the seam; largest sample step -95.7 dBFS |
| Same seam without `crossfade:` | +3.01 dB bump and a -59.9 dBFS step from summing both clips |
| Fields absent | bit-identical to the per-frame gain and linear-fade sum |
| Cache after two different alignments | two `StretchKey`s, 2 bakes, different buffers |
| 5x tempo difference | refused: required ratio 5.0000, outside 0.25..=4.0 |

## 10. GUI take lanes and comping

An audio track records one pass per loop wrap when the transport loop is armed.
Each pass writes its own file, `media/take-<track>-<n>.wav`, and its clip carries
`take_group: "<track> take <session>"` and `take: <n>`. The first pass starts at
the record position, as before. Later passes start at the loop start and end at
the loop end, so their clips tile the loop. A punch range trims every pass to
the intersection of the pass and the punch: capture outside is discarded, the
file starts at the punch in, and the clip's start is the punch in.

When the transport stops, the newest pass is unmuted and every earlier pass of
the group is muted. The track therefore sounds like the last pass until the user
comps. An empty pass writes no clip and is removed; if every pass is empty the
transport reports `Nothing was recorded: the input was silent.`

A track with a take group shows a **Takes** disclosure. Opening it adds a comp
row and one row per take:

- the comp row draws every unmuted clip of the group as a segment labelled with
  its take number, so the chosen take is visible per range;
- each take row draws that take's clips over their recorded range, dimmed where
  the take is muted;
- `M` mutes or unmutes every segment of that take as one document edit with one
  undo entry;
- `S` auditions that take against the live render only. Solo is session state
  like the mixer's audition: it never writes `muted:` and an export never hears
  it. A second press clears it.

Dragging across a take row selects that take for the dragged range. The drag
snaps both edges to the shared editing grid and calls `comp_audio_takes`. The
command splits the chosen take and every other take of the group at the range
edges where they cross, then mutes every other take's segment inside the range.
It is still the clip document: there is no second comp object, no hidden take
list, and one gesture produces one undo entry. Dragging a different take in an
adjacent range leaves the first segment alone, so successive drags build a comp.

The transport reports a loop session as `Recorded 3 takes (2:14–2:22). Take 3
is playing; open Takes to comp.` A failed write leaves the project unchanged and
names the file that could not be written.

Known limits of the GUI half:

- Passes commit when the transport stops. A crash during a loop session leaves
  the WAV files but no clips.
- One take group per track is assumed. Two recordings of the same track in one
  project show the newest group in the lane; comping a range that two groups
  cover is refused.
- A punch range is read when a pass starts; changing it mid-pass applies to the
  next pass. Changing the loop region mid-pass also applies at the next wrap.
- Comping does not add crossfades. Set `crossfade:` on the clips if a seam needs
  one (section 5).
- Take solo is not restored after a project is reopened; it is an audition, not
  document state.

## 11. Verification

```sh
cargo test -p oc-engine --lib -- align
cargo test -p oc-engine --lib -- absent_polarity
cargo test -p oc-engine --test all clips::take_alignment::
cargo test -p oc-cli --test render clip_align::
cargo test -p oc-dsl --lib -- audio_clip
cargo test -p oc-gui --lib -- record
cargo test -p oc-gui --lib -- take_lanes
```

The engine tests synthesise click trains and 100 Hz tones (no external audio),
render the clips through `audio_clip::mix_into_track_with_caches`, and measure
the detected onsets, the polarity correlation, the seam peak and the largest
sample step. The CLI test runs `ocp clip align`, checks the written fields, then
renders with `ocp render` and repeats the onset and polarity measurements; it
also checks that a refused correction leaves the project byte-identical.

## 12. Not implemented

- Automatic comp selection and automatic overlap detection: `crossfade:` is a
  fixed length per clip, not a per-seam envelope, and the mixer does not inspect
  neighbouring clips.
- Piecewise/non-linear correction (see the affine limit above).
- Alignment of a clip that is already stretched: analysis uses the raw media.
- Score/beat-aware matching; this is onset-only.
- Pitch correction; alignment changes timing only.
- Per-region polarity; `invert:` is one flag for the whole clip.
