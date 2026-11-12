# Audio devices

The application opens every output and capture stream through one owner. That
owner holds both device lists, the chosen devices, the negotiated rate and
period, the capture layout, and the health of whatever is open. This page
describes what the preferences section does, what is persisted, and which parts
need real hardware to verify.

## The device section

**Preferences → Audio** sits between the language picker and the MIDI section of
the toolbar's preferences popover. The output controls come first and the
capture controls follow.

| Control | What it changes |
|---|---|
| Output device | The device every output stream opens. "System default" follows the OS setting at each open. |
| Sample rate | The rate output streams should run at. The render thread follows the rate the stream reports it opened, so a different answer stops playback and rebuilds it. |
| Buffer size | The output callback period in frames. A period change reopens the streams and keeps the transport position. |
| Latency | Read-only: one output callback period plus the live master chain, in milliseconds and frames. |
| Input device | The device the capture stream opens. "System default" follows the OS setting. |
| Input channels | Stereo (the default) or mono. A mono device is heard on both sides; **Mono** downmixes a stereo device. |
| Monitor input | Sums capture into the output. Off by default, because a microphone through speakers feeds back. |
| Monitor gain | −24 dB to +12 dB, applied only while monitoring is on. |
| Input level | Read-only: the peak of the last capture block, with a clip mark once a sample reached full scale. |

The pickers show what is **open**, not what was requested. A selection the
device refuses leaves the previous device open, snaps the picker back and
explains why. A rate the device does not offer opens the nearest rate it does
offer, and the section says which rate that was — including a stream that opens
a rate other than the one negotiation asked for.

The section also shows, when they apply:

- The saved device is not connected, so the system default is in use.
- The device controls its own period, so the buffer choice is unavailable.
- The requested period was outside the device's range and was clamped.
- The backend refused a fixed period, so the device period is in use.
- An open stream reported a backend error.
- The settings file could not be read or written.

## Device keys

Selection is stored by a backend-stable key, never by the display name. A
renamed or duplicated device therefore keeps its stored selection. Output and
capture devices use the same scheme.

| Platform | Key |
|---|---|
| macOS | CoreAudio device UID (`kAudioDevicePropertyDeviceUID`), for example `coreaudio:AppleUSBAudioEngine:…`. |
| Windows | WASAPI endpoint id (`IMMDevice::GetId`), for example `wasapi:{0.0.0.00000000}.{…}`. |
| Linux | ALSA PCM name, for example `alsa:default`, `alsa:hw:1,0`. |

When a backend exposes no stable identifier, the key falls back to
`name:<display name>` and the device is only as stable as its name.

## Output negotiation

The rate list comes from the device's own supported configurations, filtered to
the two-channel stream the application opens. A configuration that reports a
range yields the standard rates inside that range, because the host API reports
ranges rather than discrete lists.

- A requested rate outside the list opens the nearest offered rate. Equal
  distances prefer the lower rate.
- A device that reports no configurations at all falls back to its own default
  rate.
- A requested period is clamped into the device's reported range.
- A device that reports no buffer range uses its own period, and the section
  says so.
- A backend that refuses the fixed period is retried with the device period; the
  refusal reason is shown.

The rate the stream reports when it opens is what the render thread runs at.
Ask for 96 kHz on an interface that answers 48 kHz and the section names both
rates, keeps the render thread at 48 kHz and leaves the transport rolling; a
later rebuild uses the answered rate. A device that answers a rate other than
the one the render thread is running at drops the render thread and the ring,
and the transport stays paused with the new rate in place.

## Capture format

The capture stream asks for the rate output negotiation names, so a take
normally needs no resampling. A capture device that offers another rate
substitutes the nearest one it does offer and the section names both. Capture
opens at two channels by default; a device with a single channel reports the
reduction instead of requesting a layout it cannot open.

Capture is not opened by reading the page. It opens when you choose an input
device or layout, turn monitoring on, or arm a take; the rules are in
[Preferences](preferences.md). A mono device is duplicated to both output sides
while monitoring, a stereo device keeps its image, and **Mono** downmixes it.

## Monitoring and takes

Monitoring is summed into the output callback through a lock-free ring, so a
captured frame is heard on the next output block. The ring holds about 0.19 s of
stereo at 44.1 kHz. Turning monitoring on flushes whatever the ring held, so a
click from before the switch is never heard. Because the mix happens inside the
output stream, monitoring is heard while that stream is open, which starts with
the first play or the first audition.

The record path uses a second ring, 96 000 samples, drained by the transport
poll. A take reports the frames the pump missed as "frames were dropped because
the take pump fell behind". Monitoring reports its own loss separately: frames
the output callback cannot keep up with are counted rather than written over
older audio.

Recording shifts the take by the latency the application attributes to itself:
one capture callback period plus the live output path, reported as
`input_latency_frames` in the audio view. An audio take drops that number and
the count-in from the head of the file it writes. The derivation is in
[Playback latency](playback-latency.md).

## Persistence

The choices live in `~/.opencompose/audio.json`, written atomically with the
private-file writer used for `midi.json`:

```json
{
  "output_device": "coreaudio:AppleUSBAudioEngine:…",
  "output_device_name": "USB Interface",
  "sample_rate": 48000,
  "buffer_frames": 256,
  "input_device": "coreaudio:AppleUSBAudioEngine:…",
  "input_device_name": "USB Interface",
  "input_channels": "stereo",
  "input_monitor": false,
  "input_monitor_gain_db": 0.0
}
```

`null` means "system default" for a device and "device default" for the rate
and the period. The display name is stored next to each key so a missing device
can be named.

When the stored device is absent at startup, the system default opens and the
section says which saved device is missing while keeping the stored key. Choose
the refresh button (↻) after reconnecting the device; the stored selection is
restored and the open streams move back to it.

The same choices travel in a [preferences bundle](preferences-bundle.md), which
carries them to another machine.

## Stream health

A stream whose backend reports an error pauses the transport and shows
`Audio output stopped: …`; the audio section keeps the failure as a notice. The
transport never reports *Playing* without a live output stream.

A capture stream that reports an error stops an armed take, keeps the frames it
already delivered and says why it stopped. The audio section keeps the capture
failure as an input notice until the next open succeeds.

## Limits

**No hotplug, output or input.** The audio host layer exposes no disconnect
event, and a macOS default-device stream installs no removal listener. A device
that disappears is noticed by the error callback or by the next enumeration,
never by a live listener. Capture devices are enumerated when capture is asked
for, so a microphone plugged in later appears after a refresh; nothing follows
the system default input on its own. The section therefore offers an explicit
refresh instead of promising automatic reconnection.

**Stereo only.** Every output stream opens two channels and capture opens at
most two. Multichannel interfaces, channel maps and aggregate devices with more
than two inputs or outputs are not addressed.

**No exclusive mode.** All output is shared with the system mixer. There is no
CoreAudio hog mode, no WASAPI exclusive mode and no ALSA `hw` path.

**Monitoring rides the output stream.** The capture path keeps running while the
transport is stopped, but the monitored mix is heard only while an output stream
is open. Opening the application and turning monitoring on is silent until a
play or an audition opens the output.

**Windows and Linux are compile-verified only.** The CoreAudio key lookup is
exercised on macOS; the WASAPI endpoint lookup and the ALSA PCM key type-check
but have not run on those systems.

## Verification

Headless tests cover selection, persistence, fallback, rate and period
negotiation, failed opens, stream death and the sweep of open streams, using a
fake device backend. They also cover the capture side: on-demand opens, the
monitor mix and its gain, the level meter and clip latch, a dead capture stream,
and a device that answers a different rate than negotiation asked for. Real
stream opens, callback sizes, hardware latency, device removal and the capture
path need the manual matrix:

1. macOS with the built-in output and a USB interface: switch both ways while
   playing, unplug the interface, reconnect it and refresh.
2. Windows with a 48 kHz endpoint: open the device at 44.1 kHz and confirm the
   section shows the rate that was actually opened.
3. Linux with ALSA `default` routed through PipeWire and through dmix: confirm
   the PCM names appear in the picker and that the buffer range is honoured.
4. macOS with the built-in microphone: arm a take, speak, and confirm the level
   meter moves, monitoring is heard once the transport has rolled at least once,
   and the take lands on the beat the click marks.
5. A USB interface with one input: choose stereo and confirm the section reports
   the channel reduction, then choose **Mono** and confirm capture opens.
6. Unplug the capture device while an armed take is rolling: the take stops, the
   notes and frames already captured are kept, and the section names the device
   that went away.
