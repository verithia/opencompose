# De-esser parameters

Parameters of the `de-esser` device (De-esser), in schema order. The
device schema is the source of truth: `ocp devices list --json` prints the
same table. `range` is the inclusive minimum and maximum, `default` is the
value a new device starts at, and `curve` is how the host maps a normalised
control value onto the range. Enumerated parameters list their options in
the range column.

| parameter | name | unit | range | default | curve |
|---|---|---|---|---|---|
| `threshold` | Threshold | dB | -60 … 0 | -36 | linear |
| `threshold_mode` | Threshold Mode | — | Absolute / Relative | Absolute | linear |
| `relative_threshold` | Relative Threshold | dB | -36 … 0 | -12 | linear |
| `range` | Range | dB | 0 … 24 | 6 | linear |
| `attack` | Attack | ms | 0.1 … 50 | 11 | log |
| `release` | Release | ms | 5 … 1000 | 15.5 | log |
| `high_frequency` | High Frequency | Hz | 2000 … 20000 | 7500 | log |
| `low_frequency` | Low Frequency | Hz | 2000 … 20000 | 14000 | log |
| `sibilance_band` | Sibilance Band | — | Auto / 5 kHz / 6 kHz / 7 kHz / 8 kHz / 9 kHz | Auto | linear |
| `band_estimation` | Band Estimation | — | Off / Learn | Off | linear |
| `band_override` | Band Override | Hz | 0 … 20000 | 0 | linear |
| `segment_detection` | Segment Detection | — | Off / On | Off | linear |
| `band_processing` | Band Processing | — | Wide Band / Split Band | Wide Band | linear |
| `detector` | Detector | — | Sidechain / In-band | Sidechain | linear |
| `stereo_link` | Stereo Link | % | 0 … 100 | 100 | linear |
| `lookahead` | Lookahead | ms | 0 … 15 | 0 | linear |
