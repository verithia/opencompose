---
id: translate-a-production-technique
title: Translate production advice into OpenCompose controls
when: Applying a tutorial, familiar plugin recipe or parameter value to OpenCompose synths, samplers or effects.
category: workflow
version: 1
tools: [read_skill, devices_catalog, project_extract, presets_list, edit_subtree, project_validate, render_range, audition_range, audio_compare]
---
Translate the audible operation before copying a setting. A tutorial can
teach an opening filter, a gated tail or keyed ducking without sharing our
parameter names, scaling, signal order or defaults. Preserve its intent.

## Establish the actual control
Extract the owning preset and insert chain, including inherited settings.
Use devices_catalog for its exact device ID and parameter metadata. Inspect
the relevant prefix rather than loading an unrelated complete catalog.
Check option status and notes: noOp does not process audio; partial needs its stated limits.
Record the unit, legal range, enum labels and modulation target. Distinguish
a percentage, a linear gain, decibels, semitones, cents, seconds and milliseconds.
Do not send a normalized dial position as hertz or reuse another synth's enum.

## Translate the signal behavior
A wavetable position selects a frame; it does not set oscillator pitch.
An envelope sustain is a held level, while decay and release are times.
Sampled one-shot playback may ignore note-off; a gate envelope does not.
Unison voice count does not guarantee mono compatibility. Hear the mono sum.
For synchronized motion inspect division, retrigger, phase and polarity;
matching a rate label alone does not reproduce where the accent lands.
For compression inspect detector source, threshold units and makeup gain.
For a return verify wet-only processing and the dry path before raising send.

## Calibrate by a small probe
Render one held note, repeated notes at the intended tempo and a note-off.
For sample work, probe a transient and a sustained region from the same file.
Change one control in a safe range and hear whether its direction matches
the expected operation. Then tune its amount in the actual arrangement.
Validate edits through the offered toolset; use edit_subtree for DSL edits
when a convenience setter is unavailable. Preserve unrelated preset fields.

If the required control or process is absent, name the capability gap. Offer
a specific supported approximation with its audible compromise. Do not claim
equivalence, invent a parameter, or relabel a stock sound as a new synthesis.
