# Preferences bundle

A preferences bundle is one JSON file that carries the settings this build keeps
outside a project: the theme, the interface language and scale, the audio and
MIDI device choices, the export dialogs' last-used options, the shared editing
grid, the keymap, and the recent directories. Move the file to another machine
and the same settings come back.

The bundle never carries a credential. No API key, access token, password or
authentication header is written, and an import refuses a file that names one.
It also carries no window geometry and no project content.

## Where the actions are

**Preferences → Storage → Preferences file** holds two actions:

| Action | What it does |
|---|---|
| Export preferences… | Asks for a file, then writes the current settings there. |
| Import preferences… | Asks for a file, validates it, and asks before replacing anything. |

Both actions apply immediately. An import first names how many settings it
would replace, and lists the fields it would skip, before you confirm.

## Schema

```text
opencompose.preferences.v1
```

```json
{
  "schema": "opencompose.preferences.v1",
  "exported_at": "2026-10-31T10:00:00.000Z",
  "app_version": "0.1.0",
  "theme": { "family": "catppuccin", "variant": "mocha" },
  "locale": "zh-Hans",
  "ui_scale": 1.0,
  "audio": {
    "output_device": "coreaudio:AppleUSBAudioEngine:…",
    "sample_rate": 48000,
    "buffer_size": 512
  },
  "midi": { "input": "Keystation 49", "channel": 1, "monitor": true },
  "export": {
    "format": "wav",
    "bit_depth": 24,
    "flac_level": 5,
    "normalise": true,
    "mp3_bitrate": null,
    "mp3_vbr": null
  },
  "grid": { "subdivision": 6 },
  "keymap": { "save": "Cmd+S" },
  "workspace": { "recent_directories": ["/Users/me/songs"] }
}
```

Every field is optional, and an unknown field is ignored. A file may carry one
setting or all of them. A `null` field records no preference: an import leaves
that setting alone and does not count it among the settings it replaces.

### Fields

| Field | Type | Applies to |
|---|---|---|
| `schema` | string | Required. Must be `opencompose.preferences.v1`. |
| `exported_at` | string | When the file was written. Informational. |
| `app_version` | string | The build that wrote the file. Export stamps it; a hand-written value is replaced. |
| `theme` | object | `family` and `variant`, both from the theme catalog. |
| `locale` | string | One of `en`, `zh-Hans`, `zh-Hant`, `ja`, `es`. |
| `ui_scale` | number | Interface scale, `0.7`–`1.5`. |
| `audio.output_device` | string or null | The backend-stable device key. |
| `audio.sample_rate` | number or null | Output rate in hertz, `8000`–`192000`. |
| `audio.buffer_size` | number or null | Device period in frames, `16`–`8192`. |
| `midi.input` | string or null | Input port name. |
| `midi.channel` | number or null | 1-based channel, `1`–`16`. This build reads and writes no channel preference, so it reports the field as skipped. |
| `midi.monitor` | boolean or null | Monitor live input. |
| `export.format` | string or null | `wav`, `flac` or `mp3`. |
| `export.bit_depth` | number or null | `16`, `24` or `32`. An MP3 export consumes 16-bit PCM, so an import ignores the depth for `mp3`. |
| `export.flac_level` | number or null | FLAC compression level, `0`–`8`. |
| `export.normalise` | boolean or null | Normalise to the master loudness target. |
| `export.mp3_bitrate` | number or null | MP3 CBR bitrate in kbps, `32`–`320`. Export writes it only for an `mp3` record; a bundle that names none keeps the `192` default. |
| `export.mp3_vbr` | number or null | MP3 VBR quality index, `0`–`9`, where `0` is the best. `null` asks for a constant bitrate. Export writes it only for an `mp3` record. |
| `grid.subdivision` | number or null | Divisions per quarter note: `1`, `2`, `3`, `4`, `6` or `8`. |
| `keymap` | object or null | Carried opaque. The keymap reader loads it on the next start. |
| `workspace.recent_directories` | array of strings | Read on export. This build cannot write the recent-directory list, so an import reports it as skipped. |

## Validation

An import reads the file strictly:

- A bundle larger than 1 MiB is refused before it is parsed.
- `schema` must equal `opencompose.preferences.v1`. Any other value refuses the
  whole file: `Not an OpenCompose preferences file: schema 'x' is not
  opencompose.preferences.v1.`
- A field of the wrong type refuses the whole file.
- A field whose name looks like a credential — `key`, `token`, `secret`,
  `password` and their plural or camel-case forms — refuses the whole file. The
  keymap is exempt: its entries are key bindings, not settings.
- A value outside its range, or one the running system cannot offer, is a
  skipped field with a reason. It never crashes the import and never blocks the
  other fields: an out-of-range sample rate, an unknown theme, a device that is
  not connected, a MIDI port that is not present, or an MP3 bitrate the encoder
  cannot write each appear in the report. A bundle that names `mp3` without a
  bitrate is not a skip: the import applies the `192` kbps CBR default.

Export validates the same way before it writes, and refuses to write a file it
would not import. The write goes through a temporary file in the same directory
and a rename, so a failure leaves the previous file intact and no temporary
behind.

## What an import reports

The result dialog lists the number of settings that were applied and every
field that was skipped, with the reason:

| Field | Reason |
|---|---|
| `audio.sample_rate` | `96000 Hz is not offered by 'Built-in Output'` |
| `export.mp3_bitrate` | `MP3 bitrate 999 kbps is outside 32–320` |
| `midi.channel` | `this build has no MIDI channel preference to write` |
| `workspace.recent_directories` | `this build cannot write the recent-directory list; open 1 folder(s) once to re-add them` |

Applying a field that reaches the backend can still be refused there. A rate the
device substitutes, or a period it clamps, is reported with the value the device
chose.
