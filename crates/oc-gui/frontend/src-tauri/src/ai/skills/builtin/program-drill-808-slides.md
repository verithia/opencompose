---
id: program-drill-808-slides
title: Program trap and drill 808 slides
when: Tuning a sampled 808, designing glide or retriggered bass phrases, and fitting them around kick attacks in trap or drill.
category: production
version: 1
tools: [devices_catalog, presets_list, presets_set_params, project_extract, edit_subtree, project_validate, analyze_notes, audition_range, audio_analyze]
---
Start with a steady note from the source. On a sampled 808, estimate the root
from the sustained part after the initial pitch fall, then check it against
a known note. Distortion can make a harmonic look like the root. Recheck a
cleaner section or source when the octave is ambiguous.

## Write the landing notes

- Place the stable bass notes against the chord changes and kick first.
  Compare shorter note lengths and rests before adding sidechain processing.
  The kick needs a clear attack and the bass still needs enough sustain.
- Test an isolated note, two separated notes and an overlapping pair. Use
  the actual preset envelope and mono settings. Confirm whether a transition
  carries the old voice or restarts the attack, since this changes the groove.
- Put a slide where it leads into a pitch you can hear. Try a shorter slide
  into the same landing note before increasing interval or length.

## Choose a slide mechanism the source supports

A native synth can use gliss on a phrase note. Name the intended target
with gliss=G4 when the next note alone is ambiguous. Check devices_catalog
for the actual mono and glide controls if you need overlapping-note glide.
A hosted instrument needs bend support for a continuous bend. A sampled source
that rejects the curve can use a retriggered gliss=run or a sequence of short
notes. Audition the attacks. Use an approved rendered slide clip or a
bend-capable synth when the part requires one smooth sweep.

Check the phrase with the kick at the real tempo. If the clean bass is hard to
hear on a small speaker, audition a modest harmonic layer while keeping the
fundamental stable. Compare the solo and full mix, then check headroom and
low-band energy on matching renders.
