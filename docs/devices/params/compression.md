# Compressor parameters

Parameters of the `compression` device (Compression), in schema order. The
device schema is the source of truth: `ocp devices list --json` prints the
same table. `range` is the inclusive minimum and maximum, `default` is the
value a new device starts at, and `curve` is how the host maps a normalised
control value onto the range. Enumerated parameters list their options in
the range column.

| parameter | name | unit | range | default | curve |
|---|---|---|---|---|---|
| `threshold` | Threshold | dB | -60 … 0 | -12 | linear |
| `ratio` | Ratio | ratio | 1 … 100 | 4 | log |
| `ratio_plain` | Ratio (plain 0-1) | — | 0 … 1 | 0.6 | linear |
| `attack` | Attack | ms | 0.1 … 200 | 10 | log |
| `attack_plain` | Attack (plain 0-1) | — | 0 … 1 | 0.32 | linear |
| `release` | Release | ms | 10 … 2000 | 100 | log |
| `release_plain` | Release (plain 0-1) | — | 0 … 1 | 0.44 | linear |
| `knee` | Knee | dB | 0 … 72 | 6 | linear |
| `range` | Range | dB | 0 … 60 | 60 | linear |
| `makeup` | Makeup Gain | dB | -12 … 24 | 0 | linear |
| `lookahead` | Lookahead | ms | 0 … 20 | 0 | linear |
| `max_lookahead` | Maximum Lookahead | ms | 0 … 20 | 20 | linear |
| `detector_hpf` | Detector HPF (20 = off) | Hz | 20 … 500 | 20 | log |
| `detector` | Detector (accepted; always peak) | — | 0 … 1 | 0 | linear |
| `stereo_link` | Stereo Link | % | 0 … 100 | 100 | linear |
| `hold` | Hold | ms | 0 … 500 | 0 | linear |
| `auto_release` | Auto Release (0/1) | — | 0 … 1 | 0 | linear |
| `mix` | Mix | fraction | 0 … 1 | 1 | linear |
| `auto_makeup` | Auto Makeup (0/1) | — | 0 … 1 | 0 | linear |
| `mode` | Mode (0 = legacy, 1 = modern) | — | 0 … 1 | 1 | linear |
| `sidechain_eq1_on` | Side Chain EQ 1 On | — | off / on | off | linear |
| `sidechain_eq1_type` | Side Chain EQ 1 Type | — | Bell / Low Shelf / High Shelf / Low Cut / High Cut | Bell | linear |
| `sidechain_eq1_frequency` | Side Chain EQ 1 Frequency | Hz | 20 … 20000 | 1000 | log |
| `sidechain_eq1_gain` | Side Chain EQ 1 Gain | dB | -30 … 30 | 0 | linear |
| `sidechain_eq1_q` | Side Chain EQ 1 Q | — | 0.1 … 10 | 0.707 | log |
| `sidechain_eq2_on` | Side Chain EQ 2 On | — | off / on | off | linear |
| `sidechain_eq2_type` | Side Chain EQ 2 Type | — | Bell / Low Shelf / High Shelf / Low Cut / High Cut | Bell | linear |
| `sidechain_eq2_frequency` | Side Chain EQ 2 Frequency | Hz | 20 … 20000 | 1000 | log |
| `sidechain_eq2_gain` | Side Chain EQ 2 Gain | dB | -30 … 30 | 0 | linear |
| `sidechain_eq2_q` | Side Chain EQ 2 Q | — | 0.1 … 10 | 0.707 | log |
| `sidechain_eq3_on` | Side Chain EQ 3 On | — | off / on | off | linear |
| `sidechain_eq3_type` | Side Chain EQ 3 Type | — | Bell / Low Shelf / High Shelf / Low Cut / High Cut | Bell | linear |
| `sidechain_eq3_frequency` | Side Chain EQ 3 Frequency | Hz | 20 … 20000 | 1000 | log |
| `sidechain_eq3_gain` | Side Chain EQ 3 Gain | dB | -30 … 30 | 0 | linear |
| `sidechain_eq3_q` | Side Chain EQ 3 Q | — | 0.1 … 10 | 0.707 | log |
| `sidechain_eq4_on` | Side Chain EQ 4 On | — | off / on | off | linear |
| `sidechain_eq4_type` | Side Chain EQ 4 Type | — | Bell / Low Shelf / High Shelf / Low Cut / High Cut | Bell | linear |
| `sidechain_eq4_frequency` | Side Chain EQ 4 Frequency | Hz | 20 … 20000 | 1000 | log |
| `sidechain_eq4_gain` | Side Chain EQ 4 Gain | dB | -30 … 30 | 0 | linear |
| `sidechain_eq4_q` | Side Chain EQ 4 Q | — | 0.1 … 10 | 0.707 | log |
| `sidechain_eq5_on` | Side Chain EQ 5 On | — | off / on | off | linear |
| `sidechain_eq5_type` | Side Chain EQ 5 Type | — | Bell / Low Shelf / High Shelf / Low Cut / High Cut | Bell | linear |
| `sidechain_eq5_frequency` | Side Chain EQ 5 Frequency | Hz | 20 … 20000 | 1000 | log |
| `sidechain_eq5_gain` | Side Chain EQ 5 Gain | dB | -30 … 30 | 0 | linear |
| `sidechain_eq5_q` | Side Chain EQ 5 Q | — | 0.1 … 10 | 0.707 | log |
| `sidechain_eq6_on` | Side Chain EQ 6 On | — | off / on | off | linear |
| `sidechain_eq6_type` | Side Chain EQ 6 Type | — | Bell / Low Shelf / High Shelf / Low Cut / High Cut | Bell | linear |
| `sidechain_eq6_frequency` | Side Chain EQ 6 Frequency | Hz | 20 … 20000 | 1000 | log |
| `sidechain_eq6_gain` | Side Chain EQ 6 Gain | dB | -30 … 30 | 0 | linear |
| `sidechain_eq6_q` | Side Chain EQ 6 Q | — | 0.1 … 10 | 0.707 | log |
