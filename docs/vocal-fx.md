# Vocal FX devices

Three small devices close the vocal chain around the space: `doubler`,
`exciter` and `ducked_send`. The chain order and the one-line laws are in
`docs/vocal-chain.md`; this document is the measured form, in the same shape as
`docs/vocal-comp.md` and `docs/vocal-tone.md`.

- `crates/oc-dsp/src/doubler.rs` — 2–4 modulated delay voices over a mono low end.
- `crates/oc-dsp/src/exciter.rs` — synthesised air band from 5–8 kHz content.
- `crates/oc-dsp/src/ducked_send.rs` — the space return, delayed, widened and
  ducked by the dry vocal envelope.

All measurements below are 48 kHz, stereo, from the ignored harnesses:

```
cargo test -p oc-dsp measurements_for_docs -- --ignored --nocapture
```

## Doubler

Four short delay taps, each on its own LFO, over a low end that never leaves
mono. The modulation is delay-time modulation; there is no resampling in the
voice path.

### Law

```
focus   = highpass(mono, f_focus)               # 2nd order, Q 0.707
low     = mono - focus                          # exactly complementary
base_i  = delay + (i - (n-1)/2) * spacing       # spacing <= 4 ms, 8..30 ms
lfo_i   = depth_i * sin(2*pi*f_i*t + phase_i)
drift_i = clamped read-rate offset from detune  # optional, sign alternates
voice_i = tap(focus_delay_line, base_i + lfo_i + drift_i)
wet     = low + sum(pan_i(voice_i)) / sqrt(n)
out     = (1 - mix) * dry + mix * wet
```

- The taps read a **mono** sum of the input, so the wet low band (`low`) is
  identical in both channels. Panning only distributes the high band. That is
  why mono-summing cannot notch the 100–300 Hz band; the high-pass is not the
  only defence.
- `spacing = min(4 ms, (delay - 8)/half, (30 - delay)/half)`: the spread shrinks
  near the ends of the delay range instead of leaving 8–30 ms.
- Per-voice rates are `rate * {1.00, 1.27, 0.84, 1.13}` clamped to 0.2–0.5 Hz;
  depths are `depth * {1.00, 0.86, 1.14, 0.92}` clamped to 1–3 ms. The phases
  are `{0.00, 0.37, 0.71, 0.58}` of a cycle. The multipliers are our choice:
  different rates and starting phases are what keeps two voices from locking.
- `detune` is a **read-rate** offset, not a resample of the voice. It is applied
  with alternating sign across the voices, so a pair stays centred on the dry
  pitch while each voice drifts apart and beats. The accumulated offset is
  clamped at ±60 ms; a note held long enough at 20 cents (about 5 s one way)
  stops drifting and holds its delay instead of running off the line.
- `mix` is a crossfade between the dry input and the wet path. The wet path
  always contains the dry low band, so the bottom stays mono at every mix.

### Constants

| constant | value | why |
|---|---|---|
| voices | 2–4 | two is a double, four is a small choir; more is a chorus |
| voice spacing | 4 ms | enough delay difference to decorrelate without a slap |
| LFO rate | 0.2–0.5 Hz | one cycle per 2–5 s reads as drift, not vibrato |
| LFO depth | 1–3 ms | 2 ms at 0.3 Hz is about ±6 cents of motion |
| detune | 0–20 cents | the useful edge of "a second singer", not a chorus |
| focus | 200–400 Hz | keeps the low end out of the modulated path |
| focus filter | 2nd order, Q 0.707 | complementary split; low + high reconstructs the input |

### Measured

Default setting, 2.5 s of deterministic noise:

| measurement | result | bound |
|---|---|---|
| zero-lag correlation, wet vs dry (above 300 Hz) | **0.015** | decorrelated |
| mono sum change, 100–300 Hz | **−0.292 dB** | ≤ 1 dB |
| 1 kHz pitch through two voices | **+5.2 cents** | ≤ 20 cents |

The pitch motion is the LFO: 2 ms at 0.3 Hz is 0.38 % of delay-time change,
which is the measured 5.2 cents. The detune control adds to this when set.

Per-voice base delays and LFO rates at the default delay and rate:

| voices | delays (ms) | rates (Hz) |
|---|---|---|
| 2 | 13.0, 17.0 | 0.30, 0.38 |
| 3 | 11.0, 15.0, 19.0 | 0.30, 0.38, 0.25 |
| 4 | 9.0, 13.0, 17.0, 21.0 | 0.30, 0.38, 0.25, 0.34 |

### Parameters

| id | unit | range | default | meaning |
|---|---|---|---|---|
| `voices` | none | 2–4 | 2 | number of taps |
| `delay` | ms | 8–30 | 15 | centre of the per-voice delay spread |
| `rate` | Hz | 0.2–0.5 | 0.3 | LFO rate of voice one |
| `depth` | ms | 1–3 | 2 | peak delay modulation of voice one |
| `detune` | cents | 0–20 | 0 | static read-rate detune, alternating sign |
| `focus` | Hz | 200–400 | 300 | high-pass before the taps |
| `mix` | % | 0–100 | 50 | dry/wet crossfade |

`latency_samples()` and `tail_samples()` are 0 and the delay window
respectively; the wet delay is not reported as latency. `mix = 0` is a
sample-exact bypass.

## Exciter

Air is synthesised from the 5–8 kHz band, not lifted from a shelf. The device
returns **only** the synthesised band: it is a parallel device, fed by a send
after the iron stage, not a serial insert.

### Law

```
source = bandpass(x, 4.8 kHz, 8 kHz)            # 2nd order high + low pass
u      = (10^(drive/20) - 1) * source
even   = u^2
odd    = 8 * u^3
air    = (1 - character) * even + character * odd
air    = oversample4x(air)
air    = band_limit(air, low_freq, high_freq)   # 4th order high + low pass
out    = amount * 10^(output/20) * air
```

- `drive 0` makes `u = 0`, so both laws are exactly zero; the device returns
  digital silence. `amount 0` multiplies the finished air by zero, so it is also
  exact silence. There is no dry path to leak.
- The even law is alias-safe by construction: the source band stops at 8 kHz,
  so its second harmonic stops at 16 kHz. The odd law could put a third
  harmonic at 24 kHz (Nyquist), so both laws run through the shared 4x FIR
  oversampler. There is one anti-alias path and one latency number.
- `character` is the even/odd blend. The odd law is scaled by 8 so the two ends
  of the control are comparable in level at the default drive; without the
  scale a cubic at these amplitudes would be inaudible under the square.
- The output band is a 4th-order high-pass at 9–12 kHz and a 4th-order
  low-pass at 14–20 kHz. The high-pass removes the DC the square law makes and
  the band limit is what makes it read as air instead of fizz.

### Measured

Default setting (`amount 50 %`, `drive 6 dB`, `character 0 %`) with a 5 kHz
sine at −12 dBFS:

| component | level |
|---|---|
| 10 kHz air | **−48.7 dBFS** |
| 5 kHz fundamental | **−240 dBFS** (exact null; dB floor) |
| loudest component below 5 kHz (100 Hz grid) | **−219.5 dBFS at 2 kHz** |

Alias, one tone, scanning the output band for anything that is not the wanted
harmonic:

| law | input | wanted | level | loudest other | alias |
|---|---|---|---|---|---|
| even | 7.9 kHz | 15.8 kHz | −44.6 dBFS | −177.6 dBFS at 15.725 kHz | **−133 dBc** |
| odd | 5.3 kHz | 15.9 kHz | −46.4 dBFS | −179.5 dBFS at 15.825 kHz | **−133 dBc** |

Nulls: at `amount 0` and at `drive 0` the largest output sample is **0.0**
(−240 dBFS is the print floor). The residual is exact, not "below a bound".

### Parameters

| id | unit | range | default | meaning |
|---|---|---|---|---|
| `amount` | % | 0–100 | 50 | level of the synthesised band |
| `drive` | dB | 0–24 | 6 | drive into the harmonic law |
| `character` | % | 0–100 | 0 | even-to-odd blend |
| `low_freq` | Hz | 9–12 kHz | 9.5 kHz | 4th-order high-pass on the air |
| `high_freq` | Hz | 14–20 kHz | 16 kHz | 4th-order low-pass on the air |
| `output` | dB | −12–12 | 0 | exact gain on the air |

`latency_samples()` is the shared oversampler's **16 samples** (0.33 ms at
48 kHz). `tail_samples()` is 32.

## Ducked send

The space return, not the reverb. The device sits after the existing plate/room
on the bus that the vocal already sends to; it never contains a reverb.

### Law

```
wet    = return delayed by predelay                  # wet-only, 20..40 ms
key    = 0.5 * (|key_l| + |key_r|)                  # dry vocal sidechain
level += (key - level) * (5 ms one-pole)
target = depth  if sidechain and level_db >= threshold else 0
duck  += (target - duck) * (attack or release coef) # 95% time = stated time
gain   = 10^(-duck/20)
mid/side(width) applied to wet, then gain applied to both channels
```

- The ballistics are exponential and the stated attack/release are **95 %
  times** (`tau = time/3`). At the stated release the return is within 0.6 dB of
  unity at the default depth; at 1.5× it is within 0.2 dB.
- Pre-delay is on the **audio**, not the key. The duck is driven by the dry
  vocal; delaying the return by 30 ms gives the duck a 30 ms head start
  before the wet onset, which is the same job a reverb pre-delay does. Set the
  reverb's own pre-delay to 0 when this device supplies it.
- Width is mid/side on the return only. The dry vocal never passes through the
  device, so the centre is untouched by construction.
- A missing sidechain is **no duck**, not self-keying: the return stays open.
  Self-keying a reverb return would duck it with its own tail, which is the
  opposite of the device.

### Measured

Depth 12 dB, attack 20 ms, release 300 ms, threshold −45 dB, predelay 30 ms:

| measurement | result |
|---|---|
| 95 % of the duck depth | **20.0 ms** after the key |
| 95 % of the release | **318 ms** after the key stops |
| residual at 1.5× release | **0.160 dB** |
| steady return ripple (600 ms of steady key) | **0.000 dB** |
| predelay | 1440 samples (30 ms) |
| reported latency | 0 |

The release is 318 ms rather than 300 ms because the key detector has a 5 ms
smoothing one-pole ahead of the ballistic and the measurement is on a 2 ms
block grid.

Engine integration: the same device on the return bus of the existing `reverb`
effect drops the return by **12 ± 1.5 dB** while the dry tone is present, and
recovers within 1 dB after 1.5× the release. The sidechain tap is the source
track's post-insert, pre-fader, pre-mute buffer, so no new routing exists.

### Parameters

| id | unit | range | default | meaning |
|---|---|---|---|---|
| `predelay` | ms | 20–40 | 30 | wet-only delay on the return |
| `depth` | dB | 0–24 | 9 | attenuation while the key is open |
| `attack` | ms | 5–100 | 20 | time to 95 % of the depth |
| `release` | ms | 100–1000 | 320 | time to 95 % of the return |
| `threshold` | dB | −60–0 | −45 | key level below which the return stays open |
| `width` | % | 0–200 | 120 | mid/side width of the return |

The device reports `gain_reduction_db()` and `max_gain_reduction_db()` for the
generic device meter; there is no custom panel. `latency_samples()` is 0 because
the pre-delay belongs to the wet return, not to the dry path.

## Latency

| device | latency | why |
|---|---|---|
| doubler | 0 | wet delay is not reported latency |
| exciter | 16 samples (0.33 ms at 48 kHz) | the shared 4x FIR oversampler; reported to PDC |
| ducked send | 0 | wet pre-delay is not reported latency |

## Real-time safety

All three allocate in `new` only. `process` touches scalar state, biquads and
fixed delay lines; no locks, threads or per-block allocation. The doubler
allocates one mono delay window of `30 + 10 ms`; the ducked send allocates a
stereo window of `40 ms`; the exciter has no delay line of its own beyond the
shared FIR. All three are invariant to the host block size (tests process 97-
and 480-frame blocks and require the same output).

## Failure modes

- **Doubler, detune at a held note.** The read-rate offset is clamped at
  ±60 ms. At 20 cents a note held longer than about 5 s one way stops drifting
  and holds its delay; at 5 cents the window is about 20 s. That is deliberate:
  the alternative is running off the delay line and clicking.
- **Doubler, delay at the ends.** At 8 or 30 ms the spread collapses to zero
  because the law shrinks the voice spacing to stay inside 8–30 ms; at the very
  ends the voices become one.
- **Doubler, focus too high.** A 400 Hz focus removes more of the low vocal
  from the wet path; the low band stays mono but the double gets thinner.
- **Exciter, used serially.** It returns only the air band. Inserted in series
  instead of on a send it replaces the vocal with air; this is the one wiring
  mistake the device cannot protect against.
- **Exciter, character 100 %.** The cubic law's useful band sits at the top of
  the source range; above an 8 kHz source the third harmonic is at Nyquist and
  is removed by the band limit, so high notes lose the odd character first.
- **Ducked send, no sidechain.** No duck. The return passes with only the
  pre-delay and width.
- **Ducked send, too-short release.** Release times near 100 ms pump on
  syllables: the return is back before the next word, so the room breathes with
  the lyric. The default 320 ms is the slow end that still recovers within a
  phrase.
- **Ducked send, threshold too low.** A −60 dB threshold lets room tone and
  breaths hold the return down; the vocal then has no space between phrases.

## Tests

Doubler:

- `mono_summing_the_wet_path_keeps_the_low_band_within_one_db`
- `two_default_voices_are_decorrelated_from_the_dry_signal`
- `one_voice_keeps_a_one_khz_tone_within_twenty_cents`
- `the_voice_lfos_stay_inside_the_stated_rate_and_depth_ranges`
- `zero_mix_is_an_exact_bypass_and_reports_zero_latency`
- `the_focus_high_pass_makes_the_low_end_mono_by_construction`

Exciter:

- `a_five_khz_tone_appears_above_nine_khz_and_nothing_below_five_khz`
- `zero_amount_and_zero_drive_are_exact_silence`
- `alias_content_stays_below_minus_sixty_dbc`
- `the_odd_law_runs_through_the_oversampler_and_keeps_the_alias_low`
- `the_output_trim_is_an_exact_gain_and_the_band_edges_limit_the_band`
- `the_exciter_reports_the_shared_oversampler_latency_and_is_block_size_invariant`

Ducked send:

- `the_return_drops_by_the_depth_within_the_attack`
- `the_release_returns_within_one_and_a_half_times_the_release`
- `a_steady_dry_tone_does_not_modulate_the_return`
- `no_sidechain_leaves_the_delayed_return_unchanged`
- `predelay_delays_the_return_and_the_device_reports_no_latency`
- `width_is_applied_to_the_wet_only_and_zero_width_is_mono`
- `zero_depth_is_an_exact_delayed_bypass`
- `block_size_does_not_change_the_output`

Engine: `ducked_send_rides_the_existing_reverb_return_and_releases` builds the
project from DSL, sends a track into a bus holding `reverb` then `ducked_send`,
and measures the return with and without the duck.

Schema tests pin the exposed surface: seven doubler parameters, six exciter
parameters and six ducked-send parameters.

## Listening targets

From `docs/vocal-chain.md`, with the measured result:

1. **Doubler** — mono sum within 1 dB below 300 Hz. Measured −0.292 dB; two
   default voices correlate with the dry at 0.015; a 1 kHz tone moves 5.2 cents.
2. **Exciter** — air above 9 kHz is present, nothing below 5 kHz, alias below
   −60 dBc, and zero amount is a true null. Measured −48.7 dBFS at 10 kHz,
   −219.5 dBFS below 5 kHz, −133 dBc, exact zero.
3. **Ducked send** — the return drops by the depth within the attack, returns
   within 1.5× the release, and a steady dry tone does not modulate it. Measured
   20.0 ms, 318 ms / 0.160 dB, 0.000 dB ripple.

## Deliberately out of scope

- A doubler pitch shifter or formant shift. The voices are delay taps; the
  detune is a read-rate offset, not a second pitch engine.
- Stereo widening of the doubler's low end. The focus split is the widest the
  device goes below 300 Hz, and that is the point.
- A second exciter oversampler. The odd law uses the same 4x FIR and the same
  16 samples as the iron and saturator.
- A dry path in the exciter. A dry/wet mix would let "amount 0" mean "bypass",
  and the acceptance is the opposite: zero amount is silence.
- A reverb in the ducked send. The device is the duck, the width and the
  pre-delay on someone else's return.
- A self-keyed duck. The sidechain is the dry vocal; a return that keys itself
  is a tremolo, not a duck.
- A custom GUI panel for any of the three. The schema drives all controls; the
  duck's reduction already appears in the generic gain-reduction meter.
