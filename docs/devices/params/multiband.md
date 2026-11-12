# Multiband compander parameters

Parameters of the `multiband` device (Multiband Compander), in schema order. The
device schema is the source of truth: `ocp devices list --json` prints the
same table. `range` is the inclusive minimum and maximum, `default` is the
value a new device starts at, and `curve` is how the host maps a normalised
control value onto the range. Enumerated parameters list their options in
the range column.

| parameter | name | unit | range | default | curve |
|---|---|---|---|---|---|
| `band_count` | Band Count | — | 2 Bands / 3 Bands / 4 Bands | 2 Bands | linear |
| `slope` | Slope | — | 6 dB/oct / 12 dB/oct / 24 dB/oct / 36 dB/oct / 48 dB/oct | 24 dB/oct | linear |
| `phase_mode` | Phase Mode | — | Minimum / Dynamic / Linear | Minimum | linear |
| `crossover_1` | Crossover 1 | Hz | 20 … 20000 | 1000 | log |
| `crossover_2` | Crossover 2 | Hz | 20 … 20000 | 4000 | log |
| `crossover_3` | Crossover 3 | Hz | 20 … 20000 | 10000 | log |
| `mix` | Mix | fraction | 0 … 1 | 1 | linear |
| `lookahead` | Lookahead | ms | 0 … 20 | 0 | linear |
| `band1_enabled` | Band 1 Enabled | — | off / on | on | linear |
| `band1_threshold` | Band 1 Threshold | dB | -60 … 0 | 0 | linear |
| `band1_range` | Band 1 Range | dB | -60 … 60 | 0 | linear |
| `band1_ratio` | Band 1 Ratio | ratio | 1 … 20 | 1 | log |
| `band1_attack` | Band 1 Attack | ms | 0.1 … 200 | 10 | log |
| `band1_release` | Band 1 Release | ms | 10 … 2000 | 100 | log |
| `band1_knee` | Band 1 Knee | dB | 0 … 24 | 6 | linear |
| `band1_makeup` | Band 1 Makeup | dB | -12 … 24 | 0 | linear |
| `band1_lookahead` | Band 1 Lookahead | ms | 0 … 20 | 0 | linear |
| `band1_solo` | Band 1 Solo | — | off / on | off | linear |
| `band1_mute` | Band 1 Mute | — | off / on | off | linear |
| `band2_enabled` | Band 2 Enabled | — | off / on | on | linear |
| `band2_threshold` | Band 2 Threshold | dB | -60 … 0 | 0 | linear |
| `band2_range` | Band 2 Range | dB | -60 … 60 | 0 | linear |
| `band2_ratio` | Band 2 Ratio | ratio | 1 … 20 | 1 | log |
| `band2_attack` | Band 2 Attack | ms | 0.1 … 200 | 10 | log |
| `band2_release` | Band 2 Release | ms | 10 … 2000 | 100 | log |
| `band2_knee` | Band 2 Knee | dB | 0 … 24 | 6 | linear |
| `band2_makeup` | Band 2 Makeup | dB | -12 … 24 | 0 | linear |
| `band2_lookahead` | Band 2 Lookahead | ms | 0 … 20 | 0 | linear |
| `band2_solo` | Band 2 Solo | — | off / on | off | linear |
| `band2_mute` | Band 2 Mute | — | off / on | off | linear |
| `band3_enabled` | Band 3 Enabled | — | off / on | on | linear |
| `band3_threshold` | Band 3 Threshold | dB | -60 … 0 | 0 | linear |
| `band3_range` | Band 3 Range | dB | -60 … 60 | 0 | linear |
| `band3_ratio` | Band 3 Ratio | ratio | 1 … 20 | 1 | log |
| `band3_attack` | Band 3 Attack | ms | 0.1 … 200 | 10 | log |
| `band3_release` | Band 3 Release | ms | 10 … 2000 | 100 | log |
| `band3_knee` | Band 3 Knee | dB | 0 … 24 | 6 | linear |
| `band3_makeup` | Band 3 Makeup | dB | -12 … 24 | 0 | linear |
| `band3_lookahead` | Band 3 Lookahead | ms | 0 … 20 | 0 | linear |
| `band3_solo` | Band 3 Solo | — | off / on | off | linear |
| `band3_mute` | Band 3 Mute | — | off / on | off | linear |
| `band4_enabled` | Band 4 Enabled | — | off / on | on | linear |
| `band4_threshold` | Band 4 Threshold | dB | -60 … 0 | 0 | linear |
| `band4_range` | Band 4 Range | dB | -60 … 60 | 0 | linear |
| `band4_ratio` | Band 4 Ratio | ratio | 1 … 20 | 1 | log |
| `band4_attack` | Band 4 Attack | ms | 0.1 … 200 | 10 | log |
| `band4_release` | Band 4 Release | ms | 10 … 2000 | 100 | log |
| `band4_knee` | Band 4 Knee | dB | 0 … 24 | 6 | linear |
| `band4_makeup` | Band 4 Makeup | dB | -12 … 24 | 0 | linear |
| `band4_lookahead` | Band 4 Lookahead | ms | 0 … 20 | 0 | linear |
| `band4_solo` | Band 4 Solo | — | off / on | off | linear |
| `band4_mute` | Band 4 Mute | — | off / on | off | linear |
| `detector` | Detector | — | Peak / Programme | Peak | linear |
