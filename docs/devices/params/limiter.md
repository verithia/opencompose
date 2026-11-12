# Limiter parameters

Parameters of the `limiter` device (Limiter), in schema order. The
device schema is the source of truth: `ocp devices list --json` prints the
same table. `range` is the inclusive minimum and maximum, `default` is the
value a new device starts at, and `curve` is how the host maps a normalised
control value onto the range. Enumerated parameters list their options in
the range column.

| parameter | name | unit | range | default | curve |
|---|---|---|---|---|---|
| `ceiling` | Ceiling | dB | -12 … 0 | -0.3 | linear |
| `release` | Release | ms | 5 … 500 | 50 | log |
| `release_mode` | Release Mode | — | Fast / Auto / Punch / Sustain | Auto | linear |
| `lookahead` | Lookahead | ms | 0 … 20 | 5 | linear |
| `true_peak` | True Peak | — | off / on | on | linear |
| `oversampling` | Oversampling | — | 4x / 8x / 16x | 4x | linear |
| `link` | Link | % | 0 … 100 | 100 | linear |
