---
id: design-transfer-curve
title: Design a transfer curve
when: A source needs editable asymmetric clipping, custom saturation or an authored level-to-level curve.
category: production
version: 1
tools: [read_skill, devices_catalog, project_get, project_extract, effects_add, effects_import_curve, effects_set_param, render_range, audio_analyze, audio_compare, audition_range]
---
Discover `transfer-shaper` and its asset descriptor in the device catalog.
The processor is 1×, linear-interpolated and endpoint-clamped. Do not describe
it as alias-free or silently substitute a tanh saturator for an authored curve.

Use `effects_import_curve` for an existing JSON asset. Pass its filepath,
zero-based effect index, target and the current project revision. The tool
returns a compact source hash/count/domain receipt; project and session
snapshots retain the validated curve inline. Do not paste a large sample table
into conversation or tool arguments. For a new simple curve, a small uniform
sample array through the ordinary `curve` parameter is enough. Start with
identity and add a few points; the GUI can edit sample count and individual levels.

The output is `dry_gain*x + wet_gain*curve(pre_gain*x)`. For wet proportion m,
use dry_gain=1-m and wet_gain=m. Keep the original input level when comparing
curves; matching the input loudness changes distortion. Compensate afterwards
when judging timbre. A mirrored curve covers 0..1 and mirrors negative input;
a bipolar curve covers -1..1 and can shape each polarity differently. Both
must output zero at zero. Large input gains hit the endpoint clamp.

Listen to a complete phrase, plus isolated attacks and tails. Compare the
same source before and after, checking harsh aliased highs, lost transients,
DC/asymmetry, bass weight and changes in the phrase's level. Preserve timing
and source state; revise only the curve or gains for one diagnosed cue at a
time. Use audio measurements to support listening, not as proof of musical
similarity or of equivalence to an unavailable processor.
