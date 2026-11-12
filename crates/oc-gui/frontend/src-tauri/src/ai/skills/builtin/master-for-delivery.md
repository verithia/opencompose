---
id: master-for-delivery
title: Master for delivery
when: A finished mix needs a clean master, true-peak margin, or an export suited to a delivery target.
category: mastering
version: 1
tools: [project_extract, devices_catalog, effects_add, effects_set_param, render_range, audio_analyze, audio_compare, audition_range]
---
Preserve a pre-master and define the requested file format and delivery
constraints. Use a complete or representative loud section to find tonal
balance, peaks and the loudest low-frequency hits.

Fix source problems in the mix when practical. On the master, make one
small change at a time. Use EQ for a verified broad balance issue and
limit for the desired peak control. Avoid stacking loudness tools just
because each one is available.

Compare bypass and processed at similar playback loudness. Listen for
flattened attacks, rough bass, pumping and loss of contrast between
sections. Reduce limiting or address the peaks upstream if a small
loudness gain costs too much groove.

Inspect sample peak and true peak separately. Measure an actual delivery
render when possible, including a decoded lossy copy if that is the
deliverable. The bounded render tool is before project loudness targeting
and does not certify the final exported loudness.

Choose bit depth and dither for the requested output. Dither belongs at the
final integer reduction, not on float intermediates. Check the start, loudest
section, ending and tails. Report measured values and the file actually checked.
