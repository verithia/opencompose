# EQ parameters

Parameters of the `eq` device (Parametric EQ), in schema order. The
device schema is the source of truth: `ocp devices list --json` prints the
same table. `range` is the inclusive minimum and maximum, `default` is the
value a new device starts at, and `curve` is how the host maps a normalised
control value onto the range. Enumerated parameters list their options in
the range column.

| parameter | name | unit | range | default | curve |
|---|---|---|---|---|---|
| `low_freq` | Low Frequency | Hz | 20 … 20000 | 200 | log |
| `low_gain` | Low Gain | dB | -24 … 24 | 0 | linear |
| `mid_freq` | Mid Frequency | Hz | 20 … 20000 | 1000 | log |
| `mid_gain` | Mid Gain | dB | -24 … 24 | 0 | linear |
| `q` | Mid Q | — | 0.1 … 20 | 1 | log |
| `high_freq` | High Frequency | Hz | 20 … 20000 | 8000 | log |
| `high_gain` | High Gain | dB | -24 … 24 | 0 | linear |
