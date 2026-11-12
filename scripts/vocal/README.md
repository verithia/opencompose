# Vocal corpus regression

`corpus_eval.py` measures `ocp vocal analyze` against hand-annotated real
singing, so the analyzer's accuracy is not judged only by the synthetic
signals in `crates/oc-dsp/src/vocal_analysis.rs`.

The audio is large and separately licensed, so it does not live in this
repository. The tool takes `--corpus-dir` (default `$OCP_VOCAL_CORPUS_DIR`,
then `private/corpus/vocal`, which git ignores); the exact
files, sizes, checksums and annotation formats are recorded in that
directory's `MANIFEST.md`.

## Corpora

| Corpus | Contents | Licence | Reference |
|---|---|---|---|
| Vocadito | 40 solo excerpts (7 languages), frame-level f0 and two note annotations | CC-BY-4.0 | `https://zenodo.org/records/5578807` |
| VocalSet | 20 singers, 17 techniques; 2,688 files carry note and contour annotations | CC-BY-4.0 | audio `https://zenodo.org/records/1442513`, annotations `https://zenodo.org/records/7061507` |

VocalSet's DOI `10.5281/zenodo.1492453` now resolves to a record whose only
file is the paper; the audio and the annotations live in the two records
above.

## Run

```sh
cargo build --release -p oc-cli
export OCP_BIN=target/release/ocp   # or --ocp
python3 scripts/vocal/corpus_eval.py --corpus vocadito --workers 4
python3 scripts/vocal/corpus_eval.py --corpus vocalset  --workers 4
python3 scripts/vocal/corpus_eval.py --corpus all --group technique --json-out /tmp/vocal.json
```

`--sample-rate` (default 48000) is the rate of the analysed playback buffer.
`--refresh` ignores the per-clip cache. Give every build its own `--cache-dir`
when you compare two of them: the intermediate `.ocn` is keyed on the detector
parameter stamp and `ANALYZER_VERSION`, not on the binary, so two variants that
differ only in code otherwise read each other's analyses. The cache lives in
`<cache-dir>/results`; intermediates go to
`<corpus-dir>/eval_cache/{media,derived,projects}`. Clips are analysed at 4
workers unless `--workers` says otherwise. `--every N` keeps every Nth clip
after sorting (a sampling aid for sweeps; the reported aggregates then cover
the sample, not the corpus).

`time_scale.json` holds per-singer audio/annotation speed exceptions (see
**Corpus defects**). Every run prints the exceptions it applied and a
four-row aggregate block: all clips as stored, all clips with the exception
applied, the exception clips as stored, and the exception clips applied. The
contract is judged on the corrected view; the as-stored view stays visible so
a corpus defect cannot hide.

## Metrics

Frame metrics use our 5 ms grid, mapped to the nearest annotation sample.

| Metric | Meaning |
|---|---|
| `f0_median_abs_c`, `f0_p95_abs_c` | median / p95 of `|1200 log2(ours/ref)|` over frames both sides call voiced, per clip, then median across clips |
| `f0_steady_p95_abs_c` | same, excluding ±30 ms around annotated note boundaries (window smear) |
| `octave_rate` | share of matched frames with `|cents| > 600`, pooled |
| `octave_up_rate`, `octave_down_rate` | the same split by direction (ours above / below the annotation) |
| `voiced_recall` | matched frames / annotated voiced frames |
| `false_voiced_rate` | frames we voice that the annotation leaves unvoiced; mostly ±23 ms window smear at note edges |
| `boundary_p90_ms` | p90 of |ours - ref| for note starts and ends, matched within 150 ms |
| `boundary_hit_rate` | share of all annotated boundaries matched within 50 ms |
| `false_boundary_rate` | share of our boundaries with no annotated boundary within 50 ms (the mirror of the hit rate) |

The three boundary rows read the analyzer's detected onset and offset, the
event a note actually starts and stops at. `--boundary-source reported` reads
the frame-grid span the project and the note grid use instead; the note-shape
rows (`note_ratio`, `missed_rate`, `split_rate`, `merged_rate`) always read the
reported span. The detected edges are the frame span corrected for the 0.45
window smear at a voiced/unvoiced edge, so the two views differ only at notes
that open out of silence or close into it. An internal cut carries no
correction: the placement step moves the cut itself, so the two views read the
same time there. Record both when a change moves an edge: on the same 40 clips
the reported view reads 76.76 % and 70.9 ms, the detected view 79.19 % and
64.1 ms, and the note-shape rows read 1.028 ratio, 6.0 % split and 0.8 % missed
in both.
| `note_ratio` | our notes / annotated notes |
| `missed_rate` | annotated notes with no overlapping note covering 40 % of them |
| `split_rate` | annotated notes covered by two or more of our notes (each ≥ 30 %) |
| `merged_rate` | our notes covering two or more annotated notes |

Vocadito has two note annotations. The harness defaults to `A1`; `A2` is one
flag away (`--vocadito-annotator A2`). The two annotators disagree by p90
75.5 ms on boundaries and differ by 19 % in note count, so boundary and note
count numbers below that spread are not detector failures.

## Corpus defects

VocalSet singer m9 is stored at twice the annotated speed: all 146 annotated
m9 clips have `audio / annotation` duration = 0.500 (one is 0.452), and the time
map `our t -> annotation t / 0.5` collapses the per-clip median error from
594.9 cents to 7.5 cents and the pooled octave-frame rate from 48.3 % to 3.7 %
(145/146 clips improve; the per-clip mean drops from 50.5 % to 1.4 %, so the
residual is concentrated in a few long clips). The audio is left untouched because the upstream
archives' checksums were verified; `time_scale.json` carries the measured
ratio and the harness applies it to the comparison only. The full evidence is
in `<corpus-dir>/MANIFEST.md` under "Known corpus defects". m9 alone is 66 % of
the VocalSet octave-error frames as stored, so always report the corrected and
the as-stored aggregate together.

## Gate

`--gate` reads `scripts/vocal/gate.json` and analyses the 10 short Vocadito
clips stored under `<corpus-dir>/gate`. It asserts the thresholds below and
exits non-zero on failure. It is deliberately not a `cargo test`: the data is
outside the repository and several hundred MB.

| Check | Limit | Measured with analyzer v6 |
|---|---|---|
| `f0_median_abs_c` | ≤ 6.0 | 3.22 |
| `f0_p95_abs_c` | ≤ 45.0 | 25.50 |
| worst clip `f0_p95_abs_c` | ≤ 75.0 | 47.25 |
| `octave_rate` | ≤ 2 % | 0.3 % |
| worst clip `octave_rate` | ≤ 6 % | 1.7 % |
| `boundary_p90_ms` | ≤ 100 | 58.6 |
| `boundary_hit_rate` | ≥ 65 % | 82.3 % |
| `missed_rate` | ≤ 3 % | 0.8 % |
| `split_rate` | ≤ 15 % | 6.2 % |
| `false_voiced_rate` | ≤ 50 % | 30.5 % |
| `voiced_recall` | ≥ 97 % | 99.6 % |
| `note_ratio` | 0.80–1.30 | 1.01 |

Thresholds are the measured value with margin; they catch a regression, not
small drift. They were re-derived after analyzer v2 (note segmentation waits
150 ms before cutting a pitch change) and kept unchanged for v3 (4 dB onset,
subharmonic margin 0.05) because v3 moves them in the safe direction: boundary
hit 72.2 → 74.5 %, p90 83.6 → 73.4 ms, octave unchanged (0.3 %). Analyzer v5
(the pitch-gated 125 ms fast cut) moves boundary hit 76.2 → 79.0 % and p90
69.3 → 67.4 ms at note ratio 0.97 → 1.01, so the thresholds are unchanged.
Boundary numbers
are read against the annotator ceiling (A1 vs A2: p90 75.5 ms, 84 % within
50 ms), not the synthetic 20 ms target.

```sh
python3 scripts/vocal/corpus_eval.py --gate --ocp "$OCP_BIN"
```

## Rebuilding the data

The corpus directory is produced by fetching the three Zenodo archives,
verifying the md5 checksums published by Zenodo, and extracting:

```
vocadito.zip                  -> vocadito/            (Audio/, Annotations/{F0,Notes,Lyrics})
VocalSet11.zip                -> vocalset/            (FULL/<singer>/<vocalise>/<technique>/)
"Annotated VocalSet.zip"      -> vocalset_annotations/ (extended N/, raw N/)
```

`vocalset_annotations/raw 4/csv` holds the 11.6 ms `Time, F0` contours;
`vocalset_annotations/extended 4/all files.csv` holds one row per note or rest
with start, end, measured median f0 and the intended note. The gate subset is
copied by hand; `scripts/vocal/gate.json` lists the files.
