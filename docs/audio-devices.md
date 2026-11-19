# Audio devices

One owner in the application opens every output and capture stream. It holds
both device lists, the chosen devices, the negotiated rate and period, the
capture layout, and the health of the streams that are open.

## The Audio page

Open [Preferences](preferences.md) and choose **Audio**. The output controls
come first, then the capture controls, then the MIDI input.

| Control | What it changes |
|---|---|
| Output device | The device every output stream opens. **System default** follows the OS setting each time a stream opens. |
| Sample rate | The rate output streams should run at. The render thread runs at the rate the stream reports when it opens, so a different answer stops playback and rebuilds it. |
| Buffer size | The output callback period in frames. Changing it reopens the streams and keeps the transport position. |
| Latency | Read-only: one output callback period plus the live master chain, in milliseconds and frames. |
| Input device | The device the capture stream opens. **System default** follows the OS setting. |
| Input channels | **Stereo** (the default) or **Mono**. A mono device is heard on both sides; **Mono** downmixes a stereo device. |
| Monitor input | Sums capture into the output. It is off by default, because a microphone playing through speakers feeds back. |
| Monitor gain | −24 dB to +12 dB, applied only while monitoring is on. |
| Input level | Read-only: the peak of the last capture block, with a clip mark once a sample has reached full scale. |

The pickers show the device that is open, which may differ from the one you
asked for. If a device refuses your selection, the previous device stays open,
the picker returns to it and the page gives the reason. If a device does not
offer the rate you chose, the nearest rate it does offer opens and the page
names it. This also covers a stream that opens at a rate other than the one
negotiation asked for.

The page also tells you when:

- the saved device is not connected, so the system default is in use;
- the device controls its own period, so the buffer choice is unavailable;
- the requested period was outside the device's range and was clamped;
- the backend refused a fixed period, so the device period is in use;
- an open stream reported a backend error;
- the settings file could not be read or written.

## Device keys

A selection is stored by a key that the backend keeps stable, and never by
the display name. A device that is renamed, or a second device with the same
name, therefore keeps its stored selection. Output and capture devices use the
same scheme.

| Platform | Key |
|---|---|
| macOS | CoreAudio device UID (`kAudioDevicePropertyDeviceUID`), for example `coreaudio:AppleUSBAudioEngine:…`. |
| Windows | WASAPI endpoint id (`IMMDevice::GetId`), for example `wasapi:{0.0.0.00000000}.{…}`. |
| Linux | ALSA PCM name, for example `alsa:default`, `alsa:hw:1,0`. |

When a backend exposes no stable identifier, the key falls back to
`name:<display name>`, and the selection is only as stable as the name.

## Output negotiation

The rate list comes from the device's own supported configurations, filtered
to the two-channel stream the application opens. The host API can report a
range instead of a list of rates; for a range, the list holds the standard
rates that fall inside it.

- A requested rate that is not in the list opens the nearest offered rate. On
  a tie, the lower rate wins.
- A device that reports no configurations at all falls back to its own default
  rate.
- A requested period is clamped to the device's reported range.
- A device that reports no buffer range uses its own period, and the page says
  so.
- If the backend refuses the fixed period, the stream is opened again with the
  device period, and the page shows why it was refused.

The render thread runs at the rate the stream reports when it opens. Suppose
you ask for 96 kHz and the interface answers 48 kHz: the page names both rates,
the render thread stays at 48 kHz and the transport keeps rolling. A later
rebuild uses the answered rate. If the device answers a rate other than the
one the render thread is running at, the render thread and its ring are
dropped, and the transport stays paused with the new rate in place.

## Capture format

The capture stream asks for the rate that output negotiation settled on, so a
take normally needs no resampling. If the capture device offers a different
rate, it opens the nearest one it has and the page names both. Capture opens
with two channels by default. A device with a single channel reports the
reduction and is not asked for a layout it cannot open.

Opening the page does not open capture. Capture opens when you choose an input
device or layout, turn monitoring on, or arm a take; see the capture rule in
[Preferences](preferences.md). While you monitor, a mono device is copied to
both output sides, a stereo device keeps its image, and **Mono** downmixes it.

## Monitoring and takes

Monitoring is summed into the output callback through a lock-free ring, so a
captured frame is heard in the next output block. The ring holds about 0.19 s
of stereo at 44.1 kHz. Turning monitoring on empties the ring first, so you
never hear a click captured before the switch.

The mix happens inside the output stream, so you hear monitoring only while
that stream is open. It opens with the first playback or the first audition.

The record path uses a second ring of 96 000 samples, which the transport poll
drains. A take reports frames the pump missed as "frames were dropped because
the take pump fell behind". Monitoring reports its own losses separately: when
the output callback cannot keep up, the frames are counted and older audio is
not overwritten.

Recording shifts the take by the latency the application attributes to
itself: one capture callback period plus the live output path. The audio view
reports it as `input_latency_frames`. An audio take removes that many frames,
plus the count-in, from the start of the file it writes.
[Playback latency](playback-latency.md) shows how the number is derived.

## Saved settings

Your choices are saved in `~/.opencompose/audio.json`. The file is written
atomically, with the same private-file writer as `midi.json`:

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

`null` means the system default for a device, and the device default for the
rate and the period. The display name is stored next to each key so that a
missing device can still be named.

If the saved device is missing at startup, the system default opens. The page
names the missing device and keeps its key. After you reconnect the device,
press the refresh button (↻, **Refresh audio devices**). The saved selection
comes back and the open streams move to it.

A [preferences bundle](preferences-bundle.md) carries the same choices to
another machine.

## Stream health

If an output stream's backend reports an error, the transport pauses and shows
`Audio output stopped: …`, and the Audio page keeps the failure as a notice.
The transport never reports *Playing* without a live output stream.

If a capture stream reports an error, an armed take stops. The frames already
delivered are kept, and the take says why it stopped. The Audio page keeps the
capture failure as an input notice until the next open succeeds.

## Limits

No hot-plug detection, for output or input. The audio host layer has no
disconnect event, and a macOS default-device stream installs no removal
listener. A device that disappears is noticed by the error callback or at the
next enumeration. Capture devices are enumerated when capture is requested, so
a microphone you plug in later appears after a refresh. Nothing follows the
system default input automatically. For these reasons the page has an explicit
refresh button and does not reconnect on its own.

Stereo only. Every output stream opens two channels, and capture opens at most
two. Multichannel interfaces, channel maps and aggregate devices with more than
two inputs or outputs are not supported.

No exclusive mode. All output is shared with the system mixer. There is no
CoreAudio hog mode, no WASAPI exclusive mode and no ALSA `hw` path.

Monitoring depends on the output stream. The capture path keeps running while
the transport is stopped, but you hear the monitored signal only while an
output stream is open. If you start the application and turn monitoring on, it
stays silent until playback or an audition opens the output.

Windows and Linux are only compile-checked. The CoreAudio key lookup runs on
macOS. The WASAPI endpoint lookup and the ALSA PCM key compile, but have not
been run on those systems.

## Verification

Headless tests use a fake device backend. On the output side they cover
selection, persistence, fallback, rate and period negotiation, failed opens,
stream death and the sweep of open streams. On the capture side they cover
on-demand opens, the monitor mix and its gain, the level meter and clip latch,
a dead capture stream, and a device that answers a rate other than the one
negotiation asked for.

Real stream opens, callback sizes, hardware latency, device removal and the
capture path need this manual test matrix:

1. macOS with the built-in output and a USB interface: switch both ways while
   playing, unplug the interface, reconnect it and refresh.
2. Windows with a 48 kHz endpoint: open the device at 44.1 kHz and check that
   the page shows the rate that actually opened.
3. Linux with ALSA `default` routed through PipeWire and through dmix: check
   that the PCM names appear in the picker and that the buffer range is
   respected.
4. macOS with the built-in microphone: arm a take and speak. Check that the
   level meter moves, that monitoring is heard once the transport has rolled at
   least once, and that the take lands on the beat the click marks.
5. A USB interface with one input: choose **Stereo** and check that the page
   reports the channel reduction, then choose **Mono** and check that capture
   opens.
6. Unplug the capture device while an armed take is rolling. The take stops,
   the notes and frames already captured are kept, and the page names the
   device that went away.
