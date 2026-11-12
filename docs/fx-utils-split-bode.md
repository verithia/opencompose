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

The [dimension widener and convolution reverb](fx-hyperd-conv.md) cover
additional stereo and ambience processing. The [synth effect rack](fx-laws.md)
describes how module order affects the signal.
