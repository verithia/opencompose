# Resonance suppressor

`resonance` is many-band spectral dynamics: each FFT bin is reduced only while
its power stands above the local spectral envelope, and the resulting gain
curve becomes a minimum-phase FIR. The chain position and the listening target
are in [`vocal-chain.md`](vocal-chain.md); this document is the law and the
measured form.

- `crates/oc-dsp/src/resonance.rs` — the STFT, the detector and the cepstral
  filter design.

All measurements below are 48 kHz, stereo, from the ignored harness:

```
cargo test -p oc-dsp --release resonance::tests::measurements_for_docs -- --ignored --nocapture
```

## Analysis

One 2048-point FFT every 256 samples, so eight frames overlap. The analysis and
synthesis windows are the same periodic Hann window:

```
w[n] = 0.5 - 0.5*cos(2*pi*n/2048)
sum over frames of w[n]^2 = 3        # eightfold overlap
```

The inverse FFT is unnormalised, so the frame scale is `1/(2048*3)`. The OLA
sum is exact in the real-number sense; the device does not normalise by a
measured long-term average.

A frame starting at sample `q` covers `[q, q+2048)` and can only be computed
once input sample `q+2047` exists. Its first output sample is due at input time
`q`, so the output is held one further hop to let the whole frame land in the
accumulator before that hop is read:

```
latency = 2048 + 256 = 2304 samples = 48.0 ms at 48 kHz
```

The output hop is `[T-2304, T-2304+256)` at input time `T`. That is the
documented 48 ms budget; the extra hop over the frame length is the synthesis
margin, not a second delay.

## Detection

The detector is stereo-linked. Both channels are transformed, their powers are
averaged per bin, and the same gain curve is applied to both spectra, so a
centred vocal cannot drift between channels.

```
P[k]     = (|XL[k]|^2 + |XR[k]|^2) / 2
env[k]   = mean P over the local band, excluding the +-2 bins under test
prom[k]  = 10*log10(P[k] / env[k])
target[k]= clamp(prom[k] - threshold, 0, depth)
```

`env` is a band-count proxy rather than a fixed bin count: `selectivity` maps
to 12–52 log-spaced bands over 40 Hz–16 kHz, and the band under test is left out
of its own envelope. Without the leave-one-out the peak raises its own expected
level and a single partial can never be reduced by more than the window
dilution allows; with it, `depth` behaves as a real reduction ceiling. The
default selectivity is 35 %, which is 26 bands and a one-third-octave window.

The gain curve is smoothed in time and in frequency:

- One-pole attack/release per bin, evaluated once per hop (187.5 frames per
  second at 48 kHz). `attack` is the time to 63 % of a deeper target,
  `release` the time to 63 % of the way back.
- A three-bin maximum follows the Hann main lobe, so the gain is applied to the
  whole partial rather than to its centre bin only; the measured depth then
  matches the requested depth instead of the window side coefficients
  under-reading the notch.
- A max-slope envelope: `r[k] = max(r[k], r[k+-1] - 2 dB)` in both directions.
  This is a slope limit that fills the shoulders instead of shaving the peak;
  it stops the gain alternating from bin to bin (the vocoder artefact) while
  keeping the requested depth. A 9 dB notch widens by about 4.5 bins (105 Hz at
  3 kHz), which is still more than an octave away from a 2.5 kHz neighbour.

The slope limit and the three-bin maximum never raise a bin that is not next to
a detected peak, so they do not move a component outside the detected band.

## Synthesis

The reduction curve is turned into a filter in the cepstral domain:

```
|H[k]|     = 10^(-r[k]/20)                    # r in dB, 0..24
c          = IFFT(log |H|) / 2048             # real cepstrum
c_folded   = c[0], 2*c[1..N/2-1], c[N/2], 0   # causal fold
phi        = FFT(c_folded)                    # complex log spectrum
H_min[k]   = exp(phi[k])                      # minimum-phase response
h          = IFFT(H_min)                      # the FIR
```

The full 2048 taps are kept; the FIR is 42.7 ms, inside the 48 ms budget. A
9 dB notch at 3 kHz puts 99.8 % of its impulse-response energy in the first 64
taps (centroid 1.0 tap), so the part that wraps to the start of the frame under
circular convolution is small, and the Hann window at the frame edge suppresses
it further. There is no pre-ring because a minimum-phase filter's energy is
causal. A linear-phase notch would smear energy `+-1024` samples around the
event (21 ms each side), which is exactly the pre-ring this device must not have,
and it would need another 21 ms of reported latency.

The FIR is computed once per frame and applied as a frequency-domain multiply to
both channels. `H_min` is Hermitian by construction, so the inverse transform is
real to within rounding.

`mix` crossfades that wet path with a dry path delayed by the same 2304
samples, so the two are aligned and a partial mix cannot comb. With no
reduction the wet equals the delay, so every mix position is the delayed input.
A 50 % mix on a 9 dB notch measures -3.409 dB against the -3.383 dB law.

## Parameters

| id | unit | range | default | meaning |
|---|---|---|---|---|
| `threshold` | dB | -12–18 | 6 | prominence above the envelope before a band is reduced |
| `depth` | dB | 0–24 | 9 | maximum reduction; 0 is a bit-exact bypass |
| `selectivity` | % | 0–100 | 35 | band-count proxy: 12 broad bands at 0 %, 52 narrow bands at 100 % |
| `attack` | ms | 1–100 | 10 | time to 63 % of a deeper target |
| `release` | ms | 20–1000 | 150 | time to 63 % back to zero |
| `mix` | % | 0–100 | 100 | dry/wet; 0 is a bit-exact bypass |
| `enabled` | bool | off/on | on | off is a bit-exact bypass |

A bypass (`enabled` off, `depth` 0 or `mix` 0) copies the buffer and reports
zero latency. With the wet path active, `latency_samples()` is 2304 and the
graph's PDC aligns the track.

## Measured

The acceptance signal is a white-noise bed (RMS 0.08), a 3.0 kHz ring at 0.2 and
a 2.5 kHz component at 0.003. The ring and the neighbour are measured by
quadrature projection over 2.5 s after the latency.

Reduction at 3 kHz versus the requested depth (`threshold` 6 dB, selectivity
35 %):

| requested | measured ring | 2.5 kHz move |
|---|---|---|
| 3 dB | -3.005 dB | -0.388 dB |
| 6 dB | -6.020 dB | -0.404 dB |
| 9 dB | -9.037 dB | -0.405 dB |
| 12 dB | -12.053 dB | -0.405 dB |
| 15 dB | -15.068 dB | -0.405 dB |
| 24 dB | -24.198 dB | -0.394 dB |

Depth is honoured over the whole range; the excess over the request is the
noise bed adding to the input projection of the ring.

Neighbour move versus threshold (`depth` 9 dB, selectivity 35 %):

| threshold | ring | 2.5 kHz move |
|---|---|---|
| 0 dB | -9.031 dB | -4.339 dB |
| 3 dB | -9.035 dB | -1.983 dB |
| 6 dB | -9.037 dB | -0.405 dB |
| 9 dB | -9.037 dB | +0.104 dB |
| 12 dB | -9.037 dB | +0.129 dB |

At 0–3 dB the 2.5 kHz component is itself above the local envelope, so it is
independently reduced; that is the detector working, not filter bleed. At the
default 6 dB the ring is reduced by the full depth and the neighbour moves
0.4 dB. The move is measured against the input, so it includes both any
collateral gain and the neighbour's own detection.

Selectivity (`threshold` 6 dB, `depth` 9 dB):

| selectivity | ring | 2.5 kHz move |
|---|---|---|
| 0 % (12 bands) | -9.037 dB | +0.136 dB |
| 35 % (26 bands) | -9.037 dB | -0.405 dB |
| 100 % (52 bands) | -9.037 dB | -0.788 dB |

## Latency and bypass

| state | reported latency | output |
|---|---|---|
| active | 2304 samples (48.0 ms at 48 kHz) | filtered |
| `enabled` off / `depth` 0 / `mix` 0 | 0 | bit-exact copy |

The latency test feeds a unit impulse with the threshold at its maximum (no
reduction) and finds it at 2000 + 2304 with no sample above 1e-12 before it and
the impulse amplitude preserved to 1e-6. The bypass tests compare the whole
buffer with `assert_eq!` for `mix` 0, `depth` 0 and `enabled` off.

## Cost

Measured at 48 kHz stereo in the release profile:

| item | value |
|---|---|
| CPU | 0.0165 s per audio second (1.7 %, about 60x realtime; a repeat measured 0.0141, the spread is scheduler noise) |
| debug profile (opt-level 0) | 0.591 s per audio second (0.59x realtime) |
| memory | 364,608 bytes (356 KiB) per instance |
| FFTs per frame | six 2048-point complex FFTs (two forward, two inverse, one cepstral inverse, one cepstral forward) |

The memory is five 2048-point complex buffers (160 KiB) plus the STFT, dry and
output delays and the detector state (the rest). All of it is allocated in `new`. The offline renderer runs
the release profile; one instance on a vocal track adds 1.7 % of a core to a
render whose worst track is about 6.7 %. The debug figure is why the CPU harness
is run with `--release`: at opt-level 0 rustfft is twenty times slower, not a
property of the algorithm.

Cheaper variants were measured against this one rather than assumed: a
1024/256 STFT would halve the FFT work and report 12.5 ms, and updating the
cepstral filter every other frame would save about one sixth of the cost. At
1.7 % neither is needed; if a future chain wants ten instances on one core, the
1024-point form is the variant to take (it trades low-frequency resolution, not
the law).

## Real-time safety

`new` allocates every buffer once. `process` runs the fixed 2048/256 state
machine, six FFTs through preallocated scratch, and scalar state; it takes no
locks, starts no threads and allocates nothing. `set_param` writes the
preallocated envelope bounds and coefficients. The device is invariant to the
host block size (measured with 64, 1000 and 4000-frame calls against one call).
Two fresh instances over the same input are bit-identical, and `reset` restores
a fresh instance.

## Failure modes

- **A threshold at or below the bed.** The detector is relative, so a low
  threshold reduces the broadband bed and any second partial near the ring. The
  table above quantifies it: 3 dB already moves the 2.5 kHz component by 2 dB.
  Keep the threshold above the material's own spectral fluctuation.
- **Depth is a ceiling, not a promise.** A band only receives `depth` when its
  prominence reaches `threshold + depth`. A partial buried in a dense band has
  low prominence and gets little; that is the law, not a defect.
- **One-third-octave resolution at low frequencies.** The FIR can resolve 23 Hz
  at 48 kHz, but a 60–100 Hz resonance is wider than the analysis band by
  nature; the slope limit dominates there. This device is a vocal/upper-mid
  resonance suppressor, not a low-frequency surgical EQ.
- **Transient smearing.** The gain is updated once per 256 samples and the
  frame is 42.7 ms, so a gain step is spread over a frame by construction. The
  min-phase design avoids the pre-ring; it cannot make the STFT instantaneous.
- **48 ms in the monitor path.** PDC aligns the recorded result, but during live
  monitoring the voice is 48 ms behind the ruler. Do not combine it with the
  tuner's 20–45 ms without deciding which device owns the budget.
- **Stereo-linked detection.** A resonance present in only one channel is
  detected from the average power, 3 dB lower than a centred one. That is the
  price of not moving the image.

## Tests

- `a_ring_is_reduced_by_the_requested_depth_and_the_neighbour_stays`
- `zero_mix_and_zero_depth_are_exact_bypasses_with_zero_latency`
- `the_reported_latency_is_one_window_plus_one_hop`
- `the_minimum_phase_filter_matches_the_gain_curve`
- `block_sizes_do_not_change_the_result`
- `mix_is_a_latency_aligned_crossfade`
- `two_passes_are_identical_and_reset_restores_the_state`
- `selectivity_changes_the_envelope_width`
- `extreme_settings_stay_finite_and_bounded`
- `measurements_for_docs` (ignored; the tables above)

The schema test `resonance_exposes_the_spectral_controls` pins the surface, the
engine test `resonance_factory_reports_the_stft_latency` pins the PDC number,
and `resonance_chain_reports_the_stft_latency` pins it through an effect chain.

## Deliberately out of scope

- A zero-latency biquad-bank variant. It is a different device: the detection
  would drive many time-varying biquads and the minimum-phase guarantee would be
  lost. The 48 ms form is the first cut specified in `vocal-chain.md`.
- Linear-phase spectral dynamics. It doubles the latency and pre-rings.
- Per-channel independent gains. The linked detector is what keeps a vocal
  centred; an independent variant is a different tool.
- Auto-threshold or auto-depth. The law is one threshold and one depth; an
  adaptive layer would hide which part of the spectrum is being moved.
- A solo/listen filter. The device is a suppressor, not an analyser; the
  spectrum view belongs to the dynamic EQ.
- Mid/side processing. The linked gain is applied to both channels; a mid-only
  or side-only variant is not part of the first cut.
