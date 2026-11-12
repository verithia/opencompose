# FDN reverb parameters

Parameters of the `reverb-fdn` device (FDN Reverb), in schema order. The
device schema is the source of truth: `ocp devices list --json` prints the
same table. `range` is the inclusive minimum and maximum, `default` is the
value a new device starts at, and `curve` is how the host maps a normalised
control value onto the range. Enumerated parameters list their options in
the range column.

| parameter | name | unit | range | default | curve |
|---|---|---|---|---|---|
| `decay` | Decay | ms | 200 … 10000 | 2500 | log |
| `space` | Space | % | 0 … 100 | 50 | linear |
| `decay_rate` | Decay Rate | % | 50 … 200 | 100 | linear |
| `brightness` | Brightness | % | 0 … 100 | 50 | linear |
| `character` | Character | % | 0 … 100 | 30 | linear |
| `distance` | Distance | % | 0 … 100 | 50 | linear |
| `predelay` | Pre-delay | ms | 0 … 500 | 0 | linear |
| `stereo_width` | Stereo Width | % | 0 … 120 | 100 | linear |
| `mix` | Mix | % | 0 … 100 | 30 | linear |
| `decay_band1` | Decay Band 1 | dB | -24 … 12 | 0 | linear |
| `decay_band2` | Decay Band 2 | dB | -24 … 12 | 0 | linear |
| `decay_band3` | Decay Band 3 | dB | -24 … 12 | 0 | linear |
| `decay_band4` | Decay Band 4 | dB | -24 … 12 | 0 | linear |
| `decay_band5` | Decay Band 5 | dB | -24 … 12 | 0 | linear |
| `decay_band6` | Decay Band 6 | dB | -24 … 12 | 0 | linear |
| `post_eq1_freq` | Post EQ 1 Frequency | Hz | 20 … 20000 | 100 | log |
| `post_eq1_gain` | Post EQ 1 Gain | dB | -30 … 30 | 0 | linear |
| `post_eq1_q` | Post EQ 1 Q | — | 0.05 … 20 | 1 | log |
| `post_eq1_shape` | Post EQ 1 Shape | — | Bell / Low Shelf / High Shelf / Low Cut / High Cut / Notch | Bell | linear |
| `post_eq2_freq` | Post EQ 2 Frequency | Hz | 20 … 20000 | 300 | log |
| `post_eq2_gain` | Post EQ 2 Gain | dB | -30 … 30 | 0 | linear |
| `post_eq2_q` | Post EQ 2 Q | — | 0.05 … 20 | 1 | log |
| `post_eq2_shape` | Post EQ 2 Shape | — | Bell / Low Shelf / High Shelf / Low Cut / High Cut / Notch | Bell | linear |
| `post_eq3_freq` | Post EQ 3 Frequency | Hz | 20 … 20000 | 1000 | log |
| `post_eq3_gain` | Post EQ 3 Gain | dB | -30 … 30 | 0 | linear |
| `post_eq3_q` | Post EQ 3 Q | — | 0.05 … 20 | 1 | log |
| `post_eq3_shape` | Post EQ 3 Shape | — | Bell / Low Shelf / High Shelf / Low Cut / High Cut / Notch | Bell | linear |
| `post_eq4_freq` | Post EQ 4 Frequency | Hz | 20 … 20000 | 3000 | log |
| `post_eq4_gain` | Post EQ 4 Gain | dB | -30 … 30 | 0 | linear |
| `post_eq4_q` | Post EQ 4 Q | — | 0.05 … 20 | 1 | log |
| `post_eq4_shape` | Post EQ 4 Shape | — | Bell / Low Shelf / High Shelf / Low Cut / High Cut / Notch | Bell | linear |
| `post_eq5_freq` | Post EQ 5 Frequency | Hz | 20 … 20000 | 8000 | log |
| `post_eq5_gain` | Post EQ 5 Gain | dB | -30 … 30 | 0 | linear |
| `post_eq5_q` | Post EQ 5 Q | — | 0.05 … 20 | 1 | log |
| `post_eq5_shape` | Post EQ 5 Shape | — | Bell / Low Shelf / High Shelf / Low Cut / High Cut / Notch | Bell | linear |
| `post_eq6_freq` | Post EQ 6 Frequency | Hz | 20 … 20000 | 16000 | log |
| `post_eq6_gain` | Post EQ 6 Gain | dB | -30 … 30 | 0 | linear |
| `post_eq6_q` | Post EQ 6 Q | — | 0.05 … 20 | 1 | log |
| `post_eq6_shape` | Post EQ 6 Shape | — | Bell / Low Shelf / High Shelf / Low Cut / High Cut / Notch | Bell | linear |
| `damping` | Damping | % | 0 … 100 | 50 | linear |
| `modulation` | Modulation | % | 0 … 100 | 30 | linear |
| `early_late` | Early / Late | % | 0 … 100 | 50 | linear |
| `decay_band1_freq` | Decay Band 1 Frequency | Hz | 20 … 20000 | 125 | log |
| `decay_band1_q` | Decay Band 1 Q | — | 0.1 … 10 | 0.707 | log |
| `decay_band1_shape` | Decay Band 1 Shape | — | None / Low Shelf / Bell / High Shelf | None | linear |
| `decay_band2_freq` | Decay Band 2 Frequency | Hz | 20 … 20000 | 1000 | log |
| `decay_band2_q` | Decay Band 2 Q | — | 0.1 … 10 | 0.707 | log |
| `decay_band2_shape` | Decay Band 2 Shape | — | None / Low Shelf / Bell / High Shelf | None | linear |
| `decay_band3_freq` | Decay Band 3 Frequency | Hz | 20 … 20000 | 2000 | log |
| `decay_band3_q` | Decay Band 3 Q | — | 0.1 … 10 | 0.707 | log |
| `decay_band3_shape` | Decay Band 3 Shape | — | None / Low Shelf / Bell / High Shelf | None | linear |
| `decay_band4_freq` | Decay Band 4 Frequency | Hz | 20 … 20000 | 4000 | log |
| `decay_band4_q` | Decay Band 4 Q | — | 0.1 … 10 | 0.707 | log |
| `decay_band4_shape` | Decay Band 4 Shape | — | None / Low Shelf / Bell / High Shelf | None | linear |
| `decay_band5_freq` | Decay Band 5 Frequency | Hz | 20 … 20000 | 8000 | log |
| `decay_band5_q` | Decay Band 5 Q | — | 0.1 … 10 | 0.707 | log |
| `decay_band5_shape` | Decay Band 5 Shape | — | None / Low Shelf / Bell / High Shelf | None | linear |
| `decay_band6_freq` | Decay Band 6 Frequency | Hz | 20 … 20000 | 16000 | log |
| `decay_band6_q` | Decay Band 6 Q | — | 0.1 … 10 | 0.707 | log |
| `decay_band6_shape` | Decay Band 6 Shape | — | None / Low Shelf / Bell / High Shelf | None | linear |

`brightness` is a six-band decay tilt. 0 % damps the top octaves hardest
(16 kHz RT30 is 0.75× its 50 % value), 50 % applies the baseline decay profile, 75 % is
the last measured point, and 100 % removes the tilt: every band decays at the
slowest band's 75 % rate and the tank absorption cutoff reaches 16 kHz. The
0-75 % segment is the measured table and does not change.
