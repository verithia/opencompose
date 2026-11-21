---
id: calibrate-modulation-and-trigger-state
title: Calibrate modulation and trigger state
when: A pulse, sweep or evolving patch has the wrong accent, inconsistent attacks or unpredictable motion.
category: production
version: 1
tools: [read_skill, devices_catalog, project_extract, presets_duplicate, presets_set_params, edit_subtree, project_validate, render_range, audition_range, audio_analyze, audio_compare]
---
Read translate-a-production-technique before transferring a familiar control.
Keep one source, one held note and a dry output while establishing motion.
Extract the owning preset and inspect the available modulation slots and targets.
Query devices_catalog for the exact source, target, units and support status.
Do not use a selectable noOp mode or assume every target accepts audio rate.

## Establish the range
First hear the unmodulated base. Add one source at a shallow depth.
Separate base value, source offset, polarity and depth; inspect the legal range.
Positive unipolar movement opens from a base; bipolar movement spans both sides.
A phase change moves the cycle in time; it is not a replacement for a DC offset.
Test both extrema and the midpoint before combining several routes.
Multiple routes can hit a limit even when each route alone sounds restrained.
Keep the amp open long enough to hear the intended brightness gesture.
Compare amp modulation with filter motion at matched perceived level.

## Establish the clock and trigger
Choose a repeating pulse, a single gesture or slowly drifting motion.
Test the advertised free, retriggered and one-shot modes independently.
Keep cycle rate fixed first; change starting phase to locate the accent.
For note-triggered motion compare detached, repeated and overlapping notes.
Separate envelope retrigger from reset to zero; the current level can matter.
Test note-off before a full cycle and hear whether release or looping continues.
Only then add a second source controlling depth or rate, if supported.
Constrain its endpoints so the main pulse remains identifiable.
Use smoothing for unwanted discontinuities without erasing intended steps.

## Verify the musical state
Render the same phrase twice, then from a mid-phrase seek and after reopening.
Listen for misplaced accents, altered first hits, clicks and accumulating tails.
A free or random process may vary; retain the accepted print when exact recall matters.
Compare the moving part alone, with drums, and in the complete phrase.
State which behavior was measured and which remains a supported approximation.
