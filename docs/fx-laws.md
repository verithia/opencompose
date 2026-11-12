# OpenSynth effect rack

OpenSynth sums its voices into a stereo signal and processes it through the
patch's effect rack. Module order changes the result: distortion before delay
feeds harmonics into the echoes, while distortion after delay also shapes
overlapping repeats.

| Module | Signal path |
|---|---|
| Distortion | Drive and tone feed an oversampled cascade of nonlinear stages. The mix aligns the dry path to the shaper's group delay. |
| Chorus | Modulated taps move in opposite directions. Summing taps at different delays spreads the comb response and adds movement. |
| Compressor | The detector tracks the input envelope. Threshold and ratio set the target gain, attack and release smooth its motion, and the module applies output gain after the dynamics stage. |
| Delay | Fractional delay taps follow the delay time. Feedback filters shape successive repeats, and the mix controls the direct and delayed signals. |

See [Delay and distortion](fx-delay-distortion.md),
[Synth compressor](fx-compressor.md), and [Synth FDN reverb](fx-reverb-fdn.md)
for the individual designs. See [OpenSynth](manual/synth.md) to edit and
automate the rack in the GUI.
