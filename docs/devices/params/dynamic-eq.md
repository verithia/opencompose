# Dynamic EQ parameters

Parameters of the `dynamic-eq` device (Dynamic EQ), in schema order. The
device schema is the source of truth: `ocp devices list --json` prints the
same table. `range` is the inclusive minimum and maximum, `default` is the
value a new device starts at, and `curve` is how the host maps a normalised
control value onto the range. Enumerated parameters list their options in
the range column.

| parameter | name | unit | range | default | curve |
|---|---|---|---|---|---|
| `band1_enabled` | Band 1 Enabled | — | off / on | off | linear |
| `band1_solo` | Band 1 Solo | — | off / on | off | linear |
| `band1_channel` | Band 1 Channel | — | Stereo / Mid / Side / Left / Right | Stereo | linear |
| `band1_type` | Band 1 Type | — | Bell / Low Shelf / High Shelf / Low Cut / High Cut / Notch / Band Pass | Bell | linear |
| `band1_freq` | Band 1 Frequency | Hz | 20 … 20000 | 100 | log |
| `band1_q` | Band 1 Q | — | 0.1 … 20 | 1 | log |
| `band1_gain` | Band 1 Gain | dB | -24 … 24 | 0 | linear |
| `band1_dynamic` | Band 1 Dynamic | — | off / on | off | linear |
| `band1_sidechain` | Band 1 Sidechain | — | off / on | off | linear |
| `band1_threshold` | Band 1 Threshold | dB | -60 … 0 | -30 | linear |
| `band1_range` | Band 1 Range | dB | -24 … 24 | -6 | linear |
| `band1_attack` | Band 1 Attack | ms | 0.1 … 200 | 10 | log |
| `band1_release` | Band 1 Release | ms | 5 … 2000 | 100 | log |
| `band2_enabled` | Band 2 Enabled | — | off / on | off | linear |
| `band2_solo` | Band 2 Solo | — | off / on | off | linear |
| `band2_channel` | Band 2 Channel | — | Stereo / Mid / Side / Left / Right | Stereo | linear |
| `band2_type` | Band 2 Type | — | Bell / Low Shelf / High Shelf / Low Cut / High Cut / Notch / Band Pass | Bell | linear |
| `band2_freq` | Band 2 Frequency | Hz | 20 … 20000 | 250 | log |
| `band2_q` | Band 2 Q | — | 0.1 … 20 | 1 | log |
| `band2_gain` | Band 2 Gain | dB | -24 … 24 | 0 | linear |
| `band2_dynamic` | Band 2 Dynamic | — | off / on | off | linear |
| `band2_sidechain` | Band 2 Sidechain | — | off / on | off | linear |
| `band2_threshold` | Band 2 Threshold | dB | -60 … 0 | -30 | linear |
| `band2_range` | Band 2 Range | dB | -24 … 24 | -6 | linear |
| `band2_attack` | Band 2 Attack | ms | 0.1 … 200 | 10 | log |
| `band2_release` | Band 2 Release | ms | 5 … 2000 | 100 | log |
| `band3_enabled` | Band 3 Enabled | — | off / on | off | linear |
| `band3_solo` | Band 3 Solo | — | off / on | off | linear |
| `band3_channel` | Band 3 Channel | — | Stereo / Mid / Side / Left / Right | Stereo | linear |
| `band3_type` | Band 3 Type | — | Bell / Low Shelf / High Shelf / Low Cut / High Cut / Notch / Band Pass | Bell | linear |
| `band3_freq` | Band 3 Frequency | Hz | 20 … 20000 | 500 | log |
| `band3_q` | Band 3 Q | — | 0.1 … 20 | 1 | log |
| `band3_gain` | Band 3 Gain | dB | -24 … 24 | 0 | linear |
| `band3_dynamic` | Band 3 Dynamic | — | off / on | off | linear |
| `band3_sidechain` | Band 3 Sidechain | — | off / on | off | linear |
| `band3_threshold` | Band 3 Threshold | dB | -60 … 0 | -30 | linear |
| `band3_range` | Band 3 Range | dB | -24 … 24 | -6 | linear |
| `band3_attack` | Band 3 Attack | ms | 0.1 … 200 | 10 | log |
| `band3_release` | Band 3 Release | ms | 5 … 2000 | 100 | log |
| `band4_enabled` | Band 4 Enabled | — | off / on | off | linear |
| `band4_solo` | Band 4 Solo | — | off / on | off | linear |
| `band4_channel` | Band 4 Channel | — | Stereo / Mid / Side / Left / Right | Stereo | linear |
| `band4_type` | Band 4 Type | — | Bell / Low Shelf / High Shelf / Low Cut / High Cut / Notch / Band Pass | Bell | linear |
| `band4_freq` | Band 4 Frequency | Hz | 20 … 20000 | 1000 | log |
| `band4_q` | Band 4 Q | — | 0.1 … 20 | 1 | log |
| `band4_gain` | Band 4 Gain | dB | -24 … 24 | 0 | linear |
| `band4_dynamic` | Band 4 Dynamic | — | off / on | off | linear |
| `band4_sidechain` | Band 4 Sidechain | — | off / on | off | linear |
| `band4_threshold` | Band 4 Threshold | dB | -60 … 0 | -30 | linear |
| `band4_range` | Band 4 Range | dB | -24 … 24 | -6 | linear |
| `band4_attack` | Band 4 Attack | ms | 0.1 … 200 | 10 | log |
| `band4_release` | Band 4 Release | ms | 5 … 2000 | 100 | log |
| `band5_enabled` | Band 5 Enabled | — | off / on | off | linear |
| `band5_solo` | Band 5 Solo | — | off / on | off | linear |
| `band5_channel` | Band 5 Channel | — | Stereo / Mid / Side / Left / Right | Stereo | linear |
| `band5_type` | Band 5 Type | — | Bell / Low Shelf / High Shelf / Low Cut / High Cut / Notch / Band Pass | Bell | linear |
| `band5_freq` | Band 5 Frequency | Hz | 20 … 20000 | 2000 | log |
| `band5_q` | Band 5 Q | — | 0.1 … 20 | 1 | log |
| `band5_gain` | Band 5 Gain | dB | -24 … 24 | 0 | linear |
| `band5_dynamic` | Band 5 Dynamic | — | off / on | off | linear |
| `band5_sidechain` | Band 5 Sidechain | — | off / on | off | linear |
| `band5_threshold` | Band 5 Threshold | dB | -60 … 0 | -30 | linear |
| `band5_range` | Band 5 Range | dB | -24 … 24 | -6 | linear |
| `band5_attack` | Band 5 Attack | ms | 0.1 … 200 | 10 | log |
| `band5_release` | Band 5 Release | ms | 5 … 2000 | 100 | log |
| `band6_enabled` | Band 6 Enabled | — | off / on | off | linear |
| `band6_solo` | Band 6 Solo | — | off / on | off | linear |
| `band6_channel` | Band 6 Channel | — | Stereo / Mid / Side / Left / Right | Stereo | linear |
| `band6_type` | Band 6 Type | — | Bell / Low Shelf / High Shelf / Low Cut / High Cut / Notch / Band Pass | Bell | linear |
| `band6_freq` | Band 6 Frequency | Hz | 20 … 20000 | 8000 | log |
| `band6_q` | Band 6 Q | — | 0.1 … 20 | 1 | log |
| `band6_gain` | Band 6 Gain | dB | -24 … 24 | 0 | linear |
| `band6_dynamic` | Band 6 Dynamic | — | off / on | off | linear |
| `band6_sidechain` | Band 6 Sidechain | — | off / on | off | linear |
| `band6_threshold` | Band 6 Threshold | dB | -60 … 0 | -30 | linear |
| `band6_range` | Band 6 Range | dB | -24 … 24 | -6 | linear |
| `band6_attack` | Band 6 Attack | ms | 0.1 … 200 | 10 | log |
| `band6_release` | Band 6 Release | ms | 5 … 2000 | 100 | log |
| `auto_gain` | Auto Gain | — | off / on | off | linear |
| `low_cut_enabled` | Low Cut Enabled | — | off / on | off | linear |
| `low_cut_freq` | Low Cut Frequency | Hz | 20 … 20000 | 20 | log |
| `low_cut_q` | Low Cut Q | — | 0.1 … 20 | 0.707 | log |
| `high_cut_enabled` | High Cut Enabled | — | off / on | off | linear |
| `high_cut_freq` | High Cut Frequency | Hz | 20 … 20000 | 20000 | log |
| `high_cut_q` | High Cut Q | — | 0.1 … 20 | 0.707 | log |
