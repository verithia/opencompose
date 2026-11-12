---
id: program-virtual-string-sections
title: Program a virtual string section
when: Separate virtual violin, viola, cello and bass parts, balance articulations, or make sampled strings breathe across a phrase.
category: orchestration
version: 2
tools: [tracks_list, presets_list, project_extract, edit_subtree, project_validate, render_compat_report, audition_range, audio_analyze]
---
Sketch outer voices and a moving inner part. Use separate tracks for distinct placement, dynamics or
articulation. Space low voices widely and reduce doublings that mask the moving line. Confirm what
each loaded patch plays. A GM ensemble fallback can sketch voicing and shares its sample color across
the `violin_section`, `viola_section` and `cello_section` paths.

Compare a short patch and a sustain at the intended tempo. Hear the attack and release before moving
notes. Verify hosted dynamics and expression controllers independently. When switching is unavailable,
use separate short and long presets. Keep strong accents deliberate and check that soft notes still speak.

## Four-bar transition and balance study

1. At 120 BPM, assign melody to `strings/violin`, answer to `strings/viola` and bass to `strings/cello`.
   These solo paths map to different GM programs. In bars 1–3 play the same quarter notes D5 and E5
   at fixed velocity. Start D5 at `@0` in each phrase and E5 at `@1.06`, `@1`, and `@0.94`.
   The written gates have a 30 ms gap, touch, and overlap by 30 ms respectively.
2. Use plain notes for the control. A written `articulation=legato` extends the gate, and a preset
   profile can send a patch-specific trigger. Check the actual sound before assuming a sampled transition.
3. In bar 4, use the best transition and add cello D3 and a viola answer from A3 to C4. If the cello
   or room masks the join, lower its level or shorten its release before changing melody timing.
4. Use `project_extract` and `edit_subtree`, then `project_validate` and `render_compat_report`.
   Use `tracks_list` for the violin index. Audition it alone and then the ensemble from bar 1
   to the exclusive end bar 5. Check the first attack, connections and release.
5. In a dry violin render, use `audio_analyze` with a `level_window` around a join, measured from
   the returned file's start. RMS flags holes or bumps. Listen for a repeated attack or doubled
   pitch before choosing the transition.
