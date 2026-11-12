# oc-audio

The shared processing contracts, audio-device ownership, media helpers and
private-file operations used by the graph, devices and plugin hosts.

## What it owns

Everything that processes audio — a built-in
compressor, a hosted VST3, the synth rack — implements [`AudioEffect`], so the
graph can hold a chain without knowing what is in it.

`audio_io` owns device selection, format negotiation, stream replacement and
health reporting. A native CPAL stream is created, started and destroyed on a
dedicated owner thread. Only its plain metadata and shutdown receipt cross
threads; the stream is never asserted to be `Send` or `Sync`. Dropping the
receipt waits for teardown, so stream creation and replacement belong on
control threads. Device callbacks continue to call the supplied processing
sink directly, without an owner-thread message on each audio block.

The `private_fs` and `wav` modules provide the shared private-directory,
atomic-write and WAV operations rather than separate GUI and CLI copies.

## Key types

| Type | Role |
|---|---|
| [`AudioEffect`](../api/oc_audio/trait.AudioEffect.html) | Processes stereo interleaved `f32` in place. `process_with_transport` adds host timeline context, `process_with_sidechain` adds a key input, and `supports_sidechain` reports whether the latter means anything. |
| [`PluginTransport`](../api/oc_audio/struct.PluginTransport.html) | Sample position, sample rate, tempo, meter and playing state handed to a plugin per block. With a `ProjectClock` attached, `quarter_notes` and `current_tempo` follow the project's tempo map. |
| [`PluginFormat`](../api/oc_audio/enum.PluginFormat.html) | `Vst3`, `Clap`, `Au`, `Lv2`, `Vst2`, `Internal`. |
| [`StreamSink`](../api/oc_audio/audio_io/struct.StreamSink.html) | An open device stream's metadata and control-thread teardown receipt. |

## Depends on

`oc-dsl`, for the clock type that `PluginTransport` optionally carries; CPAL
and platform device libraries for audio I/O; serialization and file utilities
for the shared metadata and media operations. It does not depend on the DSP,
engine, plugin-host or GUI crates.

## Rules

- DSP behaviour belongs in `oc-dsp`; plugin hosting belongs in `oc-host`;
  graph wiring belongs in `oc-engine`.
- Keep processing contracts independent of device and filesystem operations.
  Callback processing must not open streams, wait for teardown or perform I/O.
- A default trait method must keep the old behaviour: `process_with_transport`
  and `process_with_sidechain` fall back to `process`, so a device that does not
  care about a key input needs no code for it.

## API

[`api/oc_audio/`](../api/oc_audio/index.html).
