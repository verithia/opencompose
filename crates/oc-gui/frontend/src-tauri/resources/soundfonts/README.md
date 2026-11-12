# Bundled GM SoundFont

`FluidR3Mono_GM.sf3` is the bank the desktop bundle uses when no other
SoundFont is configured. It is a General MIDI bank with the 128 GM programs
and the GS variation presets.

| Property | Value |
|---|---|
| Source | MuseScore, `share/sound/FluidR3Mono_GM.sf3` (<https://github.com/musescore/MuseScore/tree/main/share/sound>), unchanged since commit `1a4bd0feb9378cd145418dbcb40246980ca1013d` |
| Content | MS_General 0.1, S. Christian Collins' adaptation of FluidR3Mono (Michael Cowgill), itself the mono conversion of Fluid R3 (Frank Wen) |
| SHA-256 | `2aacd036d7058d40a371846ef2f5dc5f130d648ab3837fe2626591ba49a71254` |
| Bytes | 23,712,790 |
| License | MIT; see `FluidR3Mono_License.md` |
| Presets | 197 (128 GM programs plus GS variations) |

The file name is MuseScore's; the bank's internal name is `MS_General v0.1`.
`FluidR3Mono_License.md` is MuseScore's license file for it, converted from
Latin-1 to UTF-8 with trailing whitespace removed. It keeps the acknowledgements
and copyright notices the license requires, and the app shows it on the
**Help & About** page through `THIRD_PARTY_LICENSES.txt`.

The license file says the licensing of later MS_General releases was under
discussion. That note concerns later releases; version 0.1, the one bundled
here, was published under the MIT license.

The bank is mono, which halves the shipped size without changing the
instrument list. The engine decoder reads the Ogg Vorbis samples directly, so
the file stays compressed on disk and needs no system SoundFont library.

Do not replace this file without updating the SHA-256 above, checking that
the GM programs still load through `source: soundfont`, and regenerating the
notices with `scripts/third-party-licenses.py`.
