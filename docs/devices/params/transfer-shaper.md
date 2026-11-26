# Transfer Shaper

Use Transfer Shaper to edit a level-to-level curve independently of a synth.
It runs at the original sample rate (1×). Strong drive and hard bends add
harmonics that can alias; the device does not claim alias-free processing.

Click the curve to select a sample, then drag its Output level slider or use
the keyboard. Import and export use JSON files. Opening an imported curve
preserves its original samples and text. One edit creates one undoable change.

| Control | Range | Default |
| --- | --- | --- |
| `pre_gain` | 0 … 16, linear input gain | 1 |
| `dry_gain` | 0 … 16, linear dry gain | 0 |
| `wet_gain` | 0 … 16, linear shaped gain | 1 |

For input `x`, output is `dry_gain*x + wet_gain*curve(pre_gain*x)`.
For a blend `m`, set dry gain to `1-m` and wet gain to `m`. No implicit
normalization or tone filter changes your level. The default curve is identity
inside −1 … 1 and clamps outside it. Linear interpolation joins uniformly
spaced samples. Stereo channels are independent; latency and tail are zero.

A version-1 asset contains `version`, `domain` (`odd` or `bipolar`), and
`samples`. A mirrored curve samples 0 … 1; a bipolar curve samples −1 … 1.
Use 2 … 65,537 finite values between −16 and 16. The interpolated output at
zero must be zero, so silence stays silent. Encoded assets are limited to 2 MiB.
The device catalog publishes these bounds.

```json
{"version":1,"domain":"odd","samples":[0,0.75,1]}
```

The project can store the asset inline in `curve` or reference a project-relative
JSON asset with `curve_file`. Use one declaration per effect. Save As collects
a relative asset alongside the project. The graphic editor applies to inline
curves. For a file-backed curve, the editor displays its path and the JSON asset
holds the editable samples. Agents should create a
JSON asset file and call `effects_import_curve` with its path, zero-based effect
index, target and current `expected_revision`. The importer returns a compact
hash/count/domain receipt. It refuses stale targets, malformed assets and
oversized files before changing the document. Numeric gains use ordinary effect tools.
