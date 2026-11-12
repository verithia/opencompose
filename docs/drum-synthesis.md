# Synthesised drum voices

`@kit` currently plays only sample layers. A pad with no `layer` entry is silent:
`DrumKit::from_kit` records a warning and the pad produces nothing. That makes the
first beat of a new project depend on finding a sample folder, and it means a kit
written by hand from the pad table alone cannot be heard.

This document specifies a **synthesised fallback voice per pad**, so a kit without
samples is audible, deterministic and mixable like any other source.

## Where it fits

`DrumKit` already owns the pad model: gain, pan, tuning, decay, release, one-shot
and choke are applied by the kit, not by the sample. The fallback replaces only
the *sample source* of a pad. Everything downstream — velocity curves, choke
groups, round-robin, voice stealing, the mixer — stays as it is, so a pad with
samples and a pad with a synthesised voice are interchangeable in a project.

Resolution order for a pad's voice:

1. a `layer` entry whose file decodes — unchanged behaviour;
2. an explicit `voice:` field on the pad (`kick`, `snare`, `hat_closed`,
   `hat_open`, `tom`, `clap`, `rim`);
3. the pad name, matched against the same table (so `pad kick | note: C2` works
   with no other fields);
4. the note number through the general-MIDI percussion map;
5. nothing — the pad stays silent and keeps its warning.

## Voices

All voices are generated once per voice start from closed-form envelopes and
fixed partial tables; there is no per-sample state recursion that depends on the
block size. Amplitudes are peak targets at velocity 127, before the pad's own
gain; the pad's `gain`, `decay` and `release` scale them afterwards.

| Voice | Source | Envelope | Target peak |
|---|---|---|---|
| `kick` | sine, pitch 150 Hz → 50 Hz with a 45 ms exponential glide | body 250 ms exponential; 2 ms click (band-limited noise, −18 dB) | −6 dBFS |
| `snare` | two modes at 185 Hz and 330 Hz plus high-passed noise above 2 kHz | modes 120 ms, noise 180 ms, both exponential | −10 dBFS |
| `hat_closed` | six square partials at the ratios 1, 1.34, 1.71, 2.06, 2.53, 3.11 of 320 Hz, high-passed at 7 kHz | 40 ms exponential | −16 dBFS |
| `hat_open` | same source | 320 ms exponential | −16 dBFS |
| `tom` | sine, pitch 220 Hz → the pad's `tuning`, 60 ms glide | 300 ms exponential | −12 dBFS |
| `clap` | four noise bursts 8 ms apart plus a 120 ms tail | bursts 6 ms, tail 120 ms | −14 dBFS |
| `rim` | 800 Hz and 1.7 kHz modes, 30 ms | 40 ms exponential | −18 dBFS |

Pitch follows the pad's `tuning` as a semitone offset from the voice's nominal
note, so a tuned kit still plays in key.

Noise is a per-pad xorshift stream seeded from the pad index fixed at kit build
time (not from the wall clock and not from a global RNG), so two renders of the
same project are byte-identical and a render is reproducible across machines.

## As shipped

The implementation is `crates/oc-synth/src/drumkit.rs`: `VoiceKind` resolves the
voice, `Voice::start_synth` seeds the envelope state from closed-form parameters,
and `render_synth` produces the sample. Two details differ from the table above
in the direction of being measurable rather than aspirational:

- **Calibration.** Each voice's raw waveform peaks at a different level, so a
  fixed gain per voice (`VoiceKind::calibration`) brings it to the table's target
  peak. The gains are measured, not derived: a kick's click contributes little,
  while the square-partial hats need the most.
- **Voice tails decide the render length.** `params_tail_seconds` adds the
  synthesised tail of a pad that has no layer, capped by the renderer's 30 s
  bound, so a sample-less kit is not cut off at the end of its last pattern.

A pad's `voice:` field overrides the name-based guess; an unknown name warns once
and falls through to the pad name, then to the note.

## Headroom

The targets above are ~7 dB below where the voices would sit if each were
normalised to 0 dBFS, which leaves room for four pads to hit on the same beat
without the bus compressor working for a living. `mix: vol=` on the track and the
kit's `max_voices` change nothing about the targets.

## What is deliberately not here

- **Not a sample-library replacement.** A synthesised snare is a usable default,
  not a substitute for a recorded one; a project that wants a specific sound
  still uses layers.
- **No round-robin or velocity layers for synthesised voices.** Those exist to
  hide sample repetition; a synthesised voice has none. Velocity changes level
  and, for the snare, the noise-to-body balance.
- **No physical modelling.** Membrane models would be more convincing and much
  more expensive; the target is a beat that can be made in ten seconds.

## Verification

- A kit with pads but no `layer` entries renders non-silent audio, and each pad
  name maps to its documented voice.
- Two renders of the same sample-less kit are byte-identical (the noise seed is
  deterministic).
- A kit that *does* have samples is unchanged: the existing drum tests and the
  render gate stay identical.
- A pad with neither a sample nor a recognised voice name keeps today's warning
  and stays silent.
