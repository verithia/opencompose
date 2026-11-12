# Offline vocal analysis

`ocp vocal analyze` turns one audio clip's playback buffer into a 5 ms grid of
`f0`, confidence and voicing, plus a note list. It is the analysis half of the
vocal workflow; it does not edit audio. The analyzer runs offline or
on a worker thread and never in the audio callback.

- `crates/oc-dsp/src/vocal_analysis.rs` — detector, global decode, smoothing,
  segmentation.
- `crates/oc-engine/src/vocal_analysis.rs` — media/clip lookup, `.ocn` cache,
  CLI-facing result.
- `crates/oc-cli/src/main.rs` — `ocp vocal analyze`.

## Contract

One analysed buffer produces one `VocalAnalysis`:

| Field | Meaning |
|---|---|
| `analysis_rate_hz` | Rate after decimation (about 16 kHz, or the input rate divided by an integer). |
| `hop_seconds` | Frame spacing on the output grid (5 ms by default). |
| `duration_s` | Length of the analysed playback window. |
| `frames[{time_s, f0_hz, confidence, voiced}]` | One row per hop. `time_s` is the window centre, measured from the start of the analysed buffer. |
| `notes[{start_s, end_s, onset_s, offset_s, f0_median_hz, confidence, voiced_ratio}]` | Segmented notes. `start_s`/`end_s` are the reported span on the frame grid; `onset_s`/`offset_s` are the detected edges. `voiced_ratio` is the fraction of frames in the note that the decoder voiced. |

`f0_hz` and `confidence` are exactly zero when `voiced` is false. Confidence is
the normalized-difference evidence `(1 - d'(tau))`, scaled by a frame-RMS gate,
so it means "this frame has a clear periodicity", not "this note is in tune".
A note's confidence is the mean of its voiced frames. `confidence_floor = 0.5`
is a consumer policy: a consumer should treat notes below it as "no confident
trajectory" and leave them alone. It is not a hard gate; the
global decode decides voicing.

What the analyzer declines to do:

- **Polyphony.** There is one decoded trajectory. Chords, stacked harmonies or
  a vocal over a sustained bed produce an unstable median and low confidence;
  the caller must not process them. There is no polyphonic fallback.
- **Whispers, breath and unvoiced consonants.** They have no usable period; the
  decoder reports unvoiced frames and may produce very short or no notes.
- **Regions with no confident trajectory.** A low-confidence note is reported
  with its confidence; it is not silently snapped or repaired.
- **Real-time use.** The analyzer allocates and runs FFTs; it belongs to the
  offline pass only.

Times are playback-buffer times. Because the engine analyses the clip's
playback buffer after stretch and offset, note times are clip-local and line up
with the clip's position on the timeline (`clip_start_seconds` is reported
separately).

### The reported span and the detected edges

A segmented note carries two spans, and consumers must pick the right one.

- **Reported span** (`start_s`, `end_s`): the first and one-past-last analysis
  frame of the note. It sits on the 5 ms grid, its boundaries are frame
  boundaries, and it is what the note grid, the beat report and the tuner's
  source window read. A frame flips between voiced and unvoiced part way into an
  attack or a release, so this span is up to 0.45 of the analysis window
  (21 ms at 48 kHz) wide of the event at a voiced/unvoiced edge.
- **Detected edges** (`onset_s`, `offset_s`): the same note with that window
  smear taken out, so they mark the moment the note starts and stops sounding.
  They are not on the frame grid. The boundary metric and the tuner's anchor
  read these.

The two are equal for a note that starts and ends at a pitch change inside a
voiced run, which is the common case on the grid. They differ at a note that
opens out of silence or closes into it. Keeping the reported span on the grid is
what stops the edge correction from shortening notes: the correction moves
`onset_s` later and `offset_s` earlier, and the reported span keeps its length.

`ocp vocal analyze --json` prints both pairs, and
`scripts/vocal/corpus_eval.py --boundary-source` chooses which one the boundary
metrics read.

## Detector

1. **Decimate.** The existing 31-tap Blackman-windowed sinc decimator from
   `tuner` runs at `round(input_rate / 16000)`. 48 kHz and 96 kHz land on
   exactly 16 kHz; 44.1 kHz lands on 14.7 kHz.
2. **Frame.** Hop 5 ms, window `max(40 ms, 3 / f0_min)`, which is 46.2 ms at the
   65 Hz floor. The window is rectangular for the difference function.
3. **Difference function.** One FFT of the full frame gives the autocorrelation;
   the tail beyond the analysis window is subtracted, leaving
   `r(tau) = sum_{j<W} x[j] x[j+tau]` and
   `d(tau) = e0 + e_tau - 2 r(tau)`. The cumulative mean normalization is
   `d'(tau) = d(tau) * tau / sum(d(1..tau))`.
4. **Candidates.** The first three local minima with `d' < 0.5`, interpolated
   with a parabola on the raw `d`. A later dip that is an integer multiple of
   an earlier, equally good dip (within 3 %, and not better by 0.05) is the
   same periodicity, not an alternative f0; it is dropped. Without that rule a
   clean 330 Hz tone selected its fourth subharmonic (82.5 Hz) on FFT round-off.
   A clearly better multiple (the octave-down repair case) is still kept.
5. **Global decode.** States are the candidates plus unvoiced. Per frame:
   `confidence_cost * (1 - confidence)` for a voiced state, `unvoiced_cost`
   for unvoiced. A voiced transition pays
   `jump_cost_per_cent * |cents|`, plus `octave_cost` when the jump is within
   `octave_window_cents` of 1200 cents. The path is backtracked, not thresholded
   per frame.
6. **Smooth.** A 3-frame median followed by octave repair. Repair uses the
   1.75 x / 0.57 x rule and the 20 % landing test from `docs/tuner.md`, but
   compares against the median of up to two voiced neighbours on each side
   rather than a running track. That keeps isolated octave errors while still
   accepting a sustained, real octave leap.
7. **Segment.** A note holds while the frame is within 60 cents of the segment
   median. A pitch change cuts only after it has stayed outside the band for
   150 ms, and the cut lands on the first violating frame. A fast cut may place
   the boundary after 125 ms when the excursion measures a real pitch change:
   both the excursion's median and its trailing 30 ms must sit at least
   120 cents from the segment median on the same side. A legato transition
   inside a short note therefore splits, while a same-pitch re-articulation or
   a vibrato excursion that is already returning to the band keeps the full
   hold. Vibrato leaves the band for at most half a cycle, so it folds back
   instead of splitting the note; undecided frames are kept out of the
   reference median so a slow drift cannot drag the band along. A log-energy
   rise above 4 dB over 10 ms forces a cut immediately. Unvoiced gaps up to
   15 ms are bridged when the pitch is continuous. Segments shorter than 50 ms
   are absorbed by the nearer neighbour. The test subtracts half an analysis
   window from a segment's measured length: a boundary cannot be localized
   closer than the window, so a 30 ms event measures about 55 ms.
   Every internal cut is then moved onto the pitch inflection of its
   transition. Inside an 80 ms window around the frame the hold chose, the cut
   goes to the slope-weighted centre of the pitch difference, `Σ i·|Δp| /
   Σ|Δp|`, rounded onto the frame grid; the move is capped at 60 ms. A
   transition whose peak slope stays below 1000 cents/s is a scoop rather than
   a note change, so it keeps the frame the band crossing chose. The note count
   is fixed before this step, so the cut moves the frame span of the two notes
   it separates and never adds or removes a note.

### Parameters

Defaults are in `DetectorParams` and every one feeds the cache key. Values
that the design fixed are used as given; the values chosen here were measured
with the harness below.

| Parameter | Value | Why |
|---|---|---|
| `f0_min_hz`, `f0_max_hz` | 65, 1200 | The design's vocal range. At 65 Hz the window grows to 3 cycles. |
| `hop_seconds` | 0.005 | Fixed grid for editors; measured note boundaries are 1.9–3.1 ms p90 from the synthetic truth. |
| `min_window_seconds`, `window_cycles` | 0.040, 3.0 | `max(40 ms, 3 cycles)` keeps at least three periods at the low end. |
| `candidate_threshold` | 0.5 | The design's candidate rule. `d'` above 0.5 is not a periodicity candidate. |
| `max_candidates` | 3 | The design's limit; the subharmonic rule usually leaves one. |
| `silence_rms`, `full_rms` | 1e-4, 1e-3 | −80 dBFS is unvoiced; the confidence gate reaches one at −60 dBFS. |
| `median_frames` | 3 | Removes one-frame jitter without rounding note transitions. |
| `octave_up_ratio`, `octave_down_ratio`, `octave_tolerance` | 1.75, 0.57, 0.20 | The rule in `docs/tuner.md`, applied against a local median. |
| `octave_repair_neighbors` | 2 | Two neighbours on each side are enough to tell an isolated error from a sustained leap. |
| `split_cents`, `split_frames` | 60, 30 | The hold rule with a persistence test: a change must stay 150 ms outside the band, so a trained ±90 cent vibrato (half a cycle ≈ 80 ms) cannot split a sustained note. The hold absorbs annotated notes shorter than 150 ms; the fast cut below places some of them. |
| `fast_cut_frames`, `fast_cut_cents`, `fast_cut_confirm_frames` | 25, 120, 6 | A fast cut may place a boundary after 125 ms, but only when the excursion's median and its trailing 30 ms both sit at least 120 cents from the segment median on the same side. This lets a legato transition inside a short note split while a same-pitch re-articulation or a returning vibrato keeps the 150 ms hold. Measured on Vocadito: 75.0 → 76.7 % boundary hit and 71.8 → 68.5 ms p90 at note ratio 1.00 → 1.03 and split 6.4 → 6.7 %. The frontier in **Accuracy** shows that the gate cannot reach 80 % at that count. |
| `subharmonic_margin` | 0.05 | The trailing (longest-period) candidates are removed when the cumulative mean at half their lag is within this of their own value. Swept 0.05–0.20: the aggressive forms fix subharmonic frames only by creating harmonic ones (Vocadito 1.10 → 1.24 → 1.60 → 2.14 %, VocalSet sample 1.45 → 1.94 → 2.71 → 3.55 %), so 0.05 is the safe default and it is neutral on the corpus. |
| `onset_rise_db`, `onset_window_seconds` | 4.0, 0.010 | The forced-boundary rise. Swept 4.0–6.0: Vocadito boundary hit 68.9 % at 6.0 rising to 72.4 % at 4.0 (p90 84.0 → 76.5 ms) while the note ratio moves 0.95 → 1.00 and the false-boundary rate 20.1 → 20.6 %. The 150 ms hold still absorbs short notes, so the sweep cannot restore the pre-hold 79.9 %; 4.0 is the best of the range and the trend is diminishing. |
| `min_note_seconds` | 0.050 | The design's merge rule, plus the half-window length correction above. |
| `bridge_gap_frames` | 3 | 15 ms of breath inside a note does not split it. |
| `jump_cost_per_cent` | 0.002 | A 100 cent note change costs 0.2, less than the unvoiced penalty; a semitone is cheap, an octave is not. Raising it to 0.010 cost 2 % of voiced frames and 0.44 cents at 0 dB SNR. |
| `octave_cost` | 2.0 | Larger than a 300 cent musical leap, so an octave needs sustained evidence. 1.0 and 3.0 measured no better. |
| `unvoiced_cost` | 0.6 | A frame needs confidence above 0.4 to stay voiced when continuity is free. Lowering it to 0.3 cost 1 % of voiced frames in the 0 dB run. |
| `confidence_cost` | 1.0 | Fixes the emission scale; confidence is already 0..1. |
| `octave_window_cents` | 100 | Half-width of the "this is an octave" window. |

Three constants move the cut after segmentation and are not `DetectorParams`
fields: `INFLECTION_WINDOW_SECONDS` 0.080, `INFLECTION_MIN_SLOPE_CENTS_PER_SECOND`
1000 and `INFLECTION_MAX_SHIFT_SECONDS` 0.060. They are not part of the cache
key; a change to them moves note timing, so it bumps `ANALYZER_VERSION`.

An explicit octave-down test was also measured: the cumulative mean at half a
candidate's lag was compared with the candidate's own value and the margin was
swept on both corpora. Every margin that removes subharmonic (octave-down)
frames creates at least as many harmonic (octave-up) ones.

The decoded path wins by `0.002 * cents` jump, `2.0` per octave, `0.6` per
unvoiced frame and `1.0 * (1 - confidence)` per voiced frame. At 6 dB and 0 dB
SNR, weight sets `0.002/2.0/0.6` and `0.010/2.0/0.6` differed by 0.44 cents and
2 % voiced; the lower jump cost is the default.

## Accuracy

Synthetic signals, all generated in the tests, 48 kHz unless stated. Errors are
voiced frames after skipping 100 ms at each end; note errors are medians.

| Signal | Measured | Contract |
|---|---|---|
| S1: 220 Hz + 2nd harmonic at −6 dB, 3 s | median −0.007 cents, p95 −0.006 cents, 1 note | median ≤ 2, p95 ≤ 5 |
| S4: 220 Hz + white noise, 12 dB SNR | median 0.279 cents, p95 2.10 cents, 0 octave errors | — |
| S4: 220 Hz + white noise, 6 dB SNR | median 1.47 cents, p95 8.47 cents, 0 octave errors | median ≤ 5, 0 octave errors |
| S3: 165 → 330 Hz exponential glide, 2 s | median 1.12 cents, p95 1.59 cents, 10 notes | tracking only |
| S5: C major 8-note scale, each −40 cents | 8 notes, max note error 0.016 cents, boundary p90 3.1 ms | note error ≤ 5, boundary p90 ≤ 20 ms |
| Rising 3-note phrase | 3 notes, boundary p90 16.9 ms | boundary p90 ≤ 20 ms |
| 30 ms blip between two 220 Hz notes | 2 notes (blip absorbed), durations 460 / 505 ms | merge |
| 80 ms 400 cent legato between two 220 Hz notes | 3 notes, middle note 272 Hz | split inside the hold |
| Same-pitch amplitude re-articulation | 1 note | no split |
| 100 cent legato glide over 1.2 s | 1 note | no split |
| 44.1 / 48 / 96 kHz, same 220 Hz tone | −0.0074 / −0.0070 / −0.0082 cents, spread 0.0012 | spread ≤ 0.5 cents |
| Determinism | identical `f64` bits for frames and notes | bit-identical |
| CPU | 0.0062 s per audio second, single thread, release build, load 8-9 | ≤ 0.02 |

The rising phrase's 16.9 ms boundary is the 4 dB forced onset, whose 10 ms
energy windows place the cut within about one window of the step; the pitch
path alone was within 2 ms. The corpus boundary p90 improved 84.0 → 75.5 ms,
so the trade is a real one and is taken.

### Corpus boundary edges

Vocadito, 40 clips, 4320 annotated boundaries, annotator A1, through
`scripts/vocal/corpus_eval.py`. The reported-span column is the same run with
`--boundary-source reported`, so the two columns differ only in which field the
metric reads.

| Metric | Reported span | Detected edges |
|---|---|---|
| boundary hit within 50 ms | 76.76 % | **79.19 %** |
| boundary p90 | 70.9 ms | **64.1 ms** |
| note ratio | 1.028 | 1.028 |
| missed rate | 0.8 % | 0.8 % |
| split rate | 6.0 % | 6.0 % |
| octave rate | 1.10 % | 1.10 % |

The detected edges read 2.4 points higher because a voiced/unvoiced edge keeps
the 20.8 ms flip correction in one view and not the other; at an internal cut
the two views are equal, because the placement moves the cut frame itself. The
note-shape rows are shared and read the reported span. The fast cut moved them
from the 150 ms hold's 1.00 / 6.4 % / 16.7 % to 1.03 / 6.7 % / 15.4 %, and the
placement brings split to 6.0 % and merged to 14.9 % at the same note count.
Annotator A2 reads 75.6 % / p90 66.0 ms, against 74.0 % / 68.2 ms before the
placement step; its note ratio is 1.19 because A2 annotates fewer notes, not
because the analysis changed.

The 80 % boundary target is not reachable at the note count. The hold is the
limit: 37 % of annotated notes are shorter than 150 ms, so their boundaries
cannot be placed at all, and every setting that places them adds notes. The
frontier below was measured on one build with the fast-cut parameters
overridden and a cache directory per row. The rows below the shipped one are
pre-placement; the shipped row is the final build with the placement step.
Columns are hit within 50 ms, p90, note ratio, split rate and missed rate.

| Hold / gate | hit | p90 | note ratio | split | missed |
|---|---|---|---|---|---|
| 150 ms, gate off (before) | 75.0 % | 71.8 ms | 1.003 | 6.4 % | 0.7 % |
| **125 ms, ≥120 ct, placement (shipped)** | **79.2 %** | **64.1 ms** | **1.028** | **6.0 %** | **0.8 %** |
| 125 ms, ≥120 ct (pre-placement) | 76.7 % | 68.5 ms | 1.028 | 6.7 % | 0.7 % |
| 125 ms, ≥100 ct | 77.0 % | 67.8 ms | 1.031 | 6.8 % | 0.7 % |
| 130 ms, ≥100 ct | 76.5 % | 68.5 ms | 1.023 | 6.9 % | 0.7 % |
| 110 ms, ≥100 ct | 78.3 % | 66.4 ms | 1.056 | 7.0 % | 0.8 % |
| 100 ms, ≥200 ct | 76.8 % | 69.7 ms | 1.031 | 6.7 % | 0.7 % |
| 100 ms, ≥100 ct | 79.1 % | 66.9 ms | 1.070 | 7.3 % | 0.8 % |
| 90 ms, ≥200 ct | 77.3 % | 69.5 ms | 1.038 | 6.8 % | 0.7 % |
| 90 ms, ≥150 ct | 78.4 % | 66.4 ms | 1.064 | 6.9 % | 0.7 % |
| 90 ms, ≥120 ct | 79.1 % | 64.4 ms | 1.077 | 7.4 % | 0.8 % |
| 90 ms, ≥100 ct | 79.9 % | 63.7 ms | 1.093 | 7.9 % | 0.8 % |
| 90 ms, ≥80 ct | 80.5 % | 61.9 ms | 1.117 | 8.8 % | 0.8 % |
| 90 ms, gate off | 80.6 % | 62.6 ms | 1.132 | 9.3 % | 0.9 % |

The gate removes the cheapest spurious cuts but not the intrinsic cost: at
18 frames it turns the 1.13 ratio / 9.3 % split hold into 1.09 / 7.9 % at
79.9 %, and 1.12 / 8.8 % at 80.5 %. Without placement, the best measured hit at
note ratio ≤1.03 is 76.7 %; the placement step lifts it to 79.2 % without
touching the count. Every further boundary point on the hold frontier still
costs about 1.8 % of note ratio. The count cost is intrinsic to a one-pass
hold: a boundary inside a merged short note adds a note, and the fast cuts that
measure a real pitch change are exactly those short notes. The remaining error
is therefore the hold itself, not the placement: 37 % of annotated notes are
shorter than 150 ms.

### The two-pass level model, measured and rejected

The frontier above leaves one lever open: place the boundaries inside short
notes with a low hold in a first pass, then remove the spurious splits in a
second pass so the note count returns. It was built and measured on the same 40
clips and 4320 annotated boundaries; it does not pay, and it is not shipped.

The first pass is the frontier point `fast_cut_frames` 18 (90 ms) with
`fast_cut_cents` 80, which places the short-note boundaries at 80.5 %. The
second pass merges two adjacent notes back into one when no forced onset sits on
the boundary, the two medians lie inside a merge interval, and the pitch right
of the boundary does not hold a level away from the left median for longer than
a hold. A real note change either re-attacks or holds a new level; a spurious
split does neither, so it is folded back. The interval and the hold were the
swept constants; the pass is not in `DetectorParams` because it is not shipped.
Each buildable row is a fresh build with its own cache directory, and the last
row cannot be built, because only the annotations identify which boundaries to
merge.

| Configuration | hit ≤50 ms | p90 | note ratio | split | missed |
|---|---|---|---|---|---|
| shipped before placement: 150 ms hold, fast cut 125 ms / 120 ct | 76.71 % | 68.5 ms | 1.028 | 6.71 % | 0.69 % |
| **shipped: the same cut with inflection placement** | **79.19 %** | **64.1 ms** | **1.028** | **6.02 %** | **0.83 %** |
| first pass only: 90 ms / 80 ct | 80.51 % | 61.9 ms | 1.117 | 8.75 % | 0.83 % |
| + merge, 30 ct, 150 ms hold | 79.1 % | 63.3 ms | 1.090 | 8.6 % | 0.7 % |
| + merge, 60 ct, 150 ms hold | 77.3 % | 64.5 ms | 1.060 | 8.3 % | 0.7 % |
| + merge, 100 ct, 150 ms hold | 72.3 % | 69.1 ms | 0.970 | 6.5 % | 0.7 % |
| + merge, 150 ct, 150 ms hold | 64.4 % | 80.1 ms | 0.840 | 4.3 % | 0.6 % |
| + merge, 200 ct, 150 ms hold | 55.2 % | 90.3 ms | 0.720 | 3.4 % | 0.5 % |
| + short-segment absorption at 65 ms | 78.4 % | 69.3 ms | 1.070 | 8.5 % | 0.8 % |
| + short-segment absorption at 80 ms | 73.9 % | 79.2 ms | 0.960 | 6.0 % | 0.6 % |
| perfect merge: only boundaries >120 ms from any annotation | 80.2 % | 61.3 ms | 1.042 | 6.9 % | 0.6 % |

No merge row meets the four targets together (hit ≥79 %, p90 ≤66 ms, ratio
≤1.03, split ≤7.0 %). The placement step taken after this measurement does meet
them at the shipped count, so merging is not needed. Two measurements explain
why the merge itself cannot get there, and neither is a threshold problem.

**Only 162 of the 253 excess notes are spurious splits.** The last row is the
ceiling: it merges exactly the boundaries that no annotation supports, which
only the annotations can identify. It keeps the hit rate, drops the split rate
to 6.9 %, and still lands at note ratio 1.042. The other 91 notes are real
note changes whose boundary landed 50–120 ms away. Merging those is the only
route to 1.03, and it deletes real notes.

**The split rate is a timing metric, not a note-count metric.** Merging 186
notes took the ratio from 1.117 to 1.019 and left the split rate at 8.75 %,
unchanged. An annotation counts as split when *our* boundary cuts through it, so
only merging that particular boundary helps. Of the 504 internal boundaries that
miss the 50 ms window, 340 lie 50–120 ms from a real edge: 66 % of those are
late by a median 62 ms and 34 % early by about 63 ms. That is a two-sided
placement error, so neither a merge nor a uniform shift fixes it. Pulling every
cut back to the pitch departure point measured 77.9 % with a 60 ms look-back and
71.5 % at 150 ms — worse at every window. The adaptive placement reads 79.19 %:
it measures the transition around each cut instead of applying one look-back, so
a fast step and a slow scoop are treated differently.

The first predicate, medians within a small interval, is the weakest one
available. Repeated notes at the same pitch are a real singing gesture: of the
pairs whose medians lie within 30 cents, 79 % are annotated boundaries, and
merging them costs 5.5 points of hit for 0.085 of note ratio. The second
predicate is better but not sufficient: of the boundaries with no re-attack
below 2 dB, 69 % are real boundaries. A gradient-boosted classifier over 19
frame and note features, cross-validated by clip, is the best merge any
detectable rule can reach; it scores 80 % precision at 220 merges, which still
puts the hit rate near 78 % at note ratio 1.03.

The shipped default therefore stays at `split_frames` 30 with the 125 ms /
120 ct fast cut, and the internal cut is refined to the pitch inflection of the
transition (row 96). Signed error, not note count, was what the remaining 2.4
points of hit were made of: placement recovers them at the same note count and
leaves the hold as the next lever.

The S3 note count is intentional: the 60 cent hold rule splits a 1200 cent
portamento into about ten notes. Frame pitch stays continuous; consumers that
want a single gesture merge by pitch proximity.

## Cache

The derived analysis lives in `<media cache dir>/<md5>.ocn`. The directory is
`OCP_DERIVED_CACHE_DIR`, then `OCP_MEDIA_CACHE_DIR`, then the platform media
cache directory. `--output` names an explicit file instead.

The key is:

```
StretchKey(MediaKey, target_rate, ratio_bits, stretch_mode, stretch_algo_version)
  | offset_frames | length_frames
  | ANALYZER_VERSION | detector_params_hash
```

`MediaKey` is the canonical path, mtime and size of the decoded file, so a
re-exported take misses. `offset_frames` and `length_frames` identify the
analysed playback window: two clips that share a file but start at different
offsets must not share an analysis. `StretchKey` is the same key the renderer
uses for its baked variants.

`ANALYZER_VERSION` is 5 since the pitch-gated fast cut (4 was the detected
edges; 3 was the 4 dB onset and the subharmonic-margin parameter; 2 was the
vibrato hold). The version is embedded in the stamp that names the file, so a
bump renames every entry instead of reusing it.

The file is a little-endian binary with the same header style as the other
derived caches:

```
offset  size  field
0       4     "OCNV"
4       1     container version (2)
5       1     flags (0)
6       8     access stamp (Unix nanos, informational)
14      4     target rate
18      8     source frames
26      4     ANALYZER_VERSION
30      8     detector params hash
38      8     analysis key hash
46      8     analysis rate (f64)
54      8     hop seconds (f64)
62      8     duration seconds (f64)
70      4     frame count
74      4     note count
78      25*n  frames: time f64, f0 f64, confidence f64, voiced u8
        56*m  notes: start f64, end f64, onset f64, offset f64, median f64,
              confidence f64, ratio f64
        8     FNV-1a checksum of everything above
```

A missing file is a miss. A file with a different key hash, analyzer version,
parameter hash, rate, window length or frame count is a miss. A truncated, bad-header, bad-magic, bad-f64 or checksum-mismatched file is also a
miss and is overwritten on the next run, never an error. The write goes through
a temporary file and a rename.

## CLI

```
ocp vocal analyze <project.ocp> <clip-or-track-selector> --json
ocp vocal analyze <project.ocp> <clip-or-track-selector> --json --output lead.ocn
ocp vocal analyze <project.ocp> <clip-or-track-selector>          # table
```

The selector is an `@audio_clip` name first, then a track name that owns
exactly one audio clip. `--sample-rate` defaults to 48000 and names the rate of
the analysed playback buffer. `--json` prints clip metadata, the cache result,
the notes and the frame array:

```json
{
  "clip": "lead",
  "track": "vocal",
  "sample_rate": 48000,
  "analysis_rate_hz": 16000.0,
  "hop_seconds": 0.005,
  "clip_start_seconds": 0.0,
  "duration_seconds": 2.0,
  "analyzer_version": 5,
  "cache": "miss",
  "output": "/Users/me/Library/Caches/OpenCompose/media/....ocn",
  "notes": [
    {"start_seconds": 0.023, "end_seconds": 1.0, "onset_seconds": 0.044,
     "offset_seconds": 0.979, "f0_median_hz": 220.0,
     "confidence": 0.98, "voiced_ratio": 1.0}
  ],
  "frames": [
    {"time_seconds": 0.023, "f0_hz": 220.0, "confidence": 0.98, "voiced": true}
  ]
}
```

The command is headless: it decodes the referenced media through the same media
cache as the renderer, applies the clip's stretch and offset, and writes only
the derived analysis. It does not open the GUI and does not modify the project.

## Non-goals

- No editor and no pitch shifting. The analysis feeds the note model and the
  bake in [`vocal-tune.md`](vocal-tune.md).
- No polyphonic, whisper or breath-trained model.
- No real-time analysis.
- Gain and fades are not applied to the analysed buffer: they change level, not
  the period, and they are not part of the analysis key.
