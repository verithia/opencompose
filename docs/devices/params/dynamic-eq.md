# Dynamic EQ parameters

Parameters of the `dynamic-eq` device (Dynamic EQ), in schema order. The
device schema is the source of truth: `ocp devices list --json` prints the
same table. `range` is the inclusive minimum and maximum, `default` is the
value a new device starts at, and `curve` is how the host maps a normalised
control value onto the range. Enumerated parameters list their options in
the range column.

The twelve bands share the same controls. Replace `<N>` with a band number
from 1 to 12. The default frequencies, in order, are 100, 250, 500, 1000,
2000, 8000, 12000, 60, 160, 750, 3500, and 5500 Hz.

Cut slopes apply to low-cut and high-cut shapes. Write a slope as
`band7_slope="48 dB/oct"` or use its zero-based selector index, `band7_slope=3`.
Each step adds a second-order Butterworth section. The Q setting controls the
amplitude at the corner across all four slopes.

Set `oversample=true` to run the filters at twice the project sample rate
using causal polyphase conversion. This mode has frequency-dependent phase
rotation and reports no fixed delay. Set `oversample_fir=true` for windowed-sinc
conversion with a fixed 44-sample delay that the mixer compensates on parallel
routes. FIR takes priority when both settings are enabled. Set FIR mode on the
effect so the mixer can prepare its delays before processing. Both modes are
off by default.

| parameter | name | unit | range | default | curve |
|---|---|---|---|---|---|
| `band<N>_enabled` | Enabled | — | off / on | off | linear |
| `band<N>_solo` | Solo | — | off / on | off | linear |
| `band<N>_channel` | Channel | — | Stereo / Mid / Side / Left / Right | Stereo | linear |
| `band<N>_type` | Type | — | Bell / Low Shelf / High Shelf / Low Cut / High Cut / Notch / Band Pass | Bell | linear |
| `band<N>_slope` | Cut Slope | — | 12 / 24 / 36 / 48 dB/oct | 12 dB/oct | linear |
| `band<N>_freq` | Frequency | Hz | 20 … 20000 | see above | log |
| `band<N>_q` | Q | — | 0.1 … 20 | 1 | log |
| `band<N>_gain` | Gain | dB | -24 … 24 | 0 | linear |
| `band<N>_dynamic` | Dynamic | — | off / on | off | linear |
| `band<N>_sidechain` | Sidechain | — | off / on | off | linear |
| `band<N>_threshold` | Threshold | dB | -60 … 0 | -30 | linear |
| `band<N>_range` | Range | dB | -24 … 24 | -6 | linear |
| `band<N>_attack` | Attack | ms | 0.1 … 200 | 10 | log |
| `band<N>_release` | Release | ms | 5 … 2000 | 100 | log |
| `auto_gain` | Auto Gain | — | off / on | off | linear |
| `oversample` | 2x Oversampling | — | off / on | off | linear |
| `oversample_fir` | 2x FIR Oversampling | — | off / on | off | linear |
| `output_gain` | Output Gain | dB | -24 … 24 | 0 | linear |
| `low_cut_enabled` | Low Cut Enabled | — | off / on | off | linear |
| `low_cut_freq` | Low Cut Frequency | Hz | 20 … 20000 | 20 | log |
| `low_cut_q` | Low Cut Q | — | 0.1 … 20 | 0.707 | log |
| `low_cut_slope` | Low Cut Slope | — | 12 / 24 / 36 / 48 dB/oct | 12 dB/oct | linear |
| `high_cut_enabled` | High Cut Enabled | — | off / on | off | linear |
| `high_cut_freq` | High Cut Frequency | Hz | 20 … 20000 | 20000 | log |
| `high_cut_q` | High Cut Q | — | 0.1 … 20 | 0.707 | log |
| `high_cut_slope` | High Cut Slope | — | 12 / 24 / 36 / 48 dB/oct | 12 dB/oct | linear |
