# DSL compatibility: retired effect shorthand

Five shipped example projects and the README were written against an earlier
effect vocabulary: `reverb: type=hall, size=large` and `eq: high_shelf=…`,
`low_shelf=…`, `presence=…`, `notch=…`, `low_cut=…`, `high_pass=…`,
`high_cut=…`. The parser has always accepted those keys as ordinary
`key=value` parameters; the device schemas never did, so `ocp validate` passed
and the typed DSP preflight refused the project.

The compatibility layer expands the retired keys into current device controls
at graph construction:

- The table lives in `oc_dsp::schema::SHORTHANDS` next to the device registry.
- `oc_engine::dsp::resolve_effect` expands it; parser, AST and serializer are
  unchanged.
- `ocp fmt` **keeps the shorthand in the file**. Only `ocp parse --json` shows
  the raw legacy parameters; the expanded controls exist only in the render
  graph. New projects should write the current names.
- `ocp validate`, `ocp validate-builtin-effects` and `ocp render` print the
  compatibility warnings to stderr.

## Precedence

An explicit current control always wins over a shorthand target. If an `eq`
row carries both a shorthand and the matching current control, the current
control is used and the shorthand only fills the controls that are still
unset. Two shorthands that target the same control resolve in comma order; the
first wins.

## Warning policy

Every expansion produces one warning per distinct shorthand per project. A
word shorthand is identified by its word (`type=hall` and `type=room` warn
separately); value shorthands are deduplicated by name, so several
`high_shelf=…` values in one project produce one warning. A shorthand whose
value cannot be represented (`type=cathedral`, `size=huge`,
`high_shelf=-2dB` with no frequency, `low_cut=50%`) produces one warning and no
parameter; it is never dropped silently.

## `eq` shorthand

The retired `eq` vocabulary is a band equalizer. The fixed three-band `eq`
device has no notch and no low/high cuts, so the layer renders legacy rows on
the six-band **Dynamic EQ** engine with every band's dynamic section off. That
is the only current device that represents all of the old controls.

| Legacy | Current controls | Conversion |
|---|---|---|
| `low_shelf=G@f` | band 1 (`band1_*`) | low shelf, `f` Hz, `G` dB, Q 0.707, enabled |
| `presence=G@f` | band 2 (`band2_*`) | bell, `f` Hz, `G` dB, enabled |
| `notch=G@f` | band 3 (`band3_*`) | notch, `f` Hz, `G` dB, Q 4.0, enabled |
| `high_shelf=G@f` | band 4 (`band4_*`) | high shelf, `f` Hz, `G` dB, Q 0.707, enabled |
| `low_cut=f` | `low_cut_enabled`, `low_cut_freq` | enabled low cut at `f` Hz |
| `high_pass=f` | `low_cut_enabled`, `low_cut_freq` | same as `low_cut` |
| `high_cut=f` | `high_cut_enabled`, `high_cut_freq` | enabled high cut at `f` Hz |

Explicit `eq` controls map onto the same bands and take precedence:
`low_freq`/`low_gain` → band 1, `mid_freq`/`mid_gain`/`q` → band 2,
`high_freq`/`high_gain` → band 4.

## `reverb` shorthand

The reverb device exposes damping, modulation, early/late, stereo width, decay
and pre-delay, but has no room-model or size control. `type` sets the character
controls and `size` sets the scale controls; they do not overlap, so a row may
use both.

| Legacy | Current controls | Conversion |
|---|---|---|
| `type=hall` | `damping`, `modulation`, `early_late` | 25 %, 20 %, 20 % |
| `type=plate` | `damping`, `modulation`, `early_late` | 10 %, 35 %, 5 % |
| `type=room` | `damping`, `modulation`, `early_late` | 45 %, 10 %, 60 % |
| `size=small` | `decay`, `predelay`, `stereo_width` | 0.7 s, 8 ms, 90 % |
| `size=medium` | `decay`, `predelay`, `stereo_width` | 1.8 s, 20 ms, 100 % |
| `size=large` | `decay`, `predelay`, `stereo_width` | 3.0 s, 30 ms, 115 % |

## Preflight scope

`ocp validate-builtin-effects` checks built-in DSP only. It skips external
plugin effects instead of refusing the project, so a project that mixes
built-in and CLAP/VST3 effects can be preflighted; rendering still loads the
plugins and fails if one is missing.
