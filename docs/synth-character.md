# OpenSynth voice design

OpenSynth renders oscillator and sample sources through per-voice modulation,
filters, and the amplitude envelope. The stereo voice sum feeds the patch
effects, then the project mixer.

## Oscillators

Wavetables store frames that represent single cycles. The oscillator
interpolates between adjacent frames as you move its position. Band-limited
levels reduce aliasing as the pitch rises. During a glide the reader
crossfades between levels to keep the waveform continuous.

Phase warp changes where the oscillator reads within a cycle. The effect
depends on the table as well as the warp mode and amount. The sub oscillator
can reinforce the fundamental and the noise source adds unpitched energy.
See [Granular and spectral oscillators](osc-granular-spectral.md) for
sample-based oscillator modes.

## Unison, filters, and modulation

Unison spreads oscillator voices in pitch and stereo position. Its level law
compensates for the number of voices. Sample layers use a separate unison gain
law. See [Synth output levels](synth-levels.md).

Voice filters provide state-variable and ladder responses with drive,
resonance, and optional key tracking. The routing controls determine which
sources each filter processes. Envelopes, LFOs, note properties, and macros
modulate the available destinations. See [Voice filter character](filter-character.md)
and [Modulation](modulation.md).

## Samples and effects

A sampler layer resolves the sample or SFZ against the project's asset
directory. A region keeps its source level, root pitch, and loop settings.
See [Sampler](sampler.md) for supported opcodes and asset resolution.

After the voice sum, the [effect rack](fx-laws.md) processes the stereo patch
output. See [OpenSynth](manual/synth.md) for the editor controls and preset
import workflow.
