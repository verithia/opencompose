# Utility, split and frequency-shift effects

OpenSynth's rack supports utility processing, signal splits, and frequency
shifting. Use the rack's order controls to place these stages before or after
other effects.

- **Utility** adjusts gain, pan, stereo width, and the passband. Its filters let
  you limit what reaches the next stage.
- **Split** separates frequency bands, and **Split MS** separates the mid and
  side signals. The branches are recombined in the rack.
- **Bode** shifts the spectrum by a fixed number of hertz. Unlike a pitch
  transpose, a fixed shift moves every partial by the same frequency interval.
  Use small shifts to add movement and wider shifts for inharmonic textures.

The mixer also has a **Frequency Shifter** insert. Set the signed shift in
hertz and select each channel's sideband. A sideband of 100% selects the upper
sideband for a positive shift, -100% selects the lower sideband, and zero
combines them for ring modulation with the internal oscillator. Phase controls
use percentages of a cycle. The mixer compensates the insert's 31-sample
latency so the dry/wet blend and parallel tracks stay aligned.

```ocp
@track texture | instrument: audio/clip
  effects:
    - frequency_shift: shift=180Hz, sideband_left=100%, sideband_right=-100%, mix=60%
```

The [dimension widener and convolution reverb](fx-hyperd-conv.md) cover
additional stereo and ambience processing. The [synth effect rack](fx-laws.md)
describes how module order affects the signal.
