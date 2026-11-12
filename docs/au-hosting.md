# AudioUnit hosting

OpenCompose hosts AudioUnit (AUv2) components in process for offline render on
macOS. AU instruments and effects sit beside the CLAP and VST3 paths: same
instance lifecycle, same MIDI and parameter plumbing, same render cache.

## What is hosted

| Component type | Code   | Behaviour                                         |
|----------------|--------|---------------------------------------------------|
| Music device   | `aumu` | MIDI in, stereo audio out (instruments)           |
| Effect         | `aufx` | stereo audio in/out, MIDI in when the unit takes it |
| Music effect   | `aumf` | stereo audio in/out plus MIDI in                  |

Every bus is negotiated as non-interleaved 32-bit float stereo at the project
sample rate, with `kAudioUnitProperty_MaximumFramesPerSlice` set to the render
block ceiling. Instruments are pulled with `AudioUnitRender` on output bus 0;
effects receive their input through a render callback on input bus 0.

## Naming a unit in a project

```ocp
@preset dls_piano
  source: au
  plugin: "Apple: DLSMusicDevice"

@preset labelled
  source: au
  plugin: "aumu:dls:appl"          // type:subtype:manufacturer

@preset bundled
  source: au
  plugin: "/Library/Audio/Plug-Ins/Components/Some Synth.component"
```

Resolution order is four-character codes, then a bundle path (read from the
bundle's `Contents/Info.plist`), then the display name the AudioComponent
registry reports. A display name matches the full name, the part after the
manufacturer (`DLSMusicDevice`), or the manufacturer; an ambiguous match is an
error that lists the candidates.

Effects use the plugin-effect syntax with an `au.` prefix:

```ocp
@track pad
  effects:
    - plugin:"au.Apple: AUReverb2"
```

An AU preset's numeric `params` are applied in the unit's own engineering
units (Hz, dB, percent) through `AudioUnitSetParameter`, matched by parameter
name with case and punctuation ignored, so the unit's `Dry/Wet Mix` is written
`drywetmix`. A parameter name that the unit does not expose is ignored.

## MIDI, transport and timing

- Note, controller, program-change, pitch-bend and pressure events are queued
  per block and delivered with `MusicDeviceMIDIEvent`, so they stay
  sample-accurate inside a render block.
- When a unit negotiates MIDI 2.0 (`kAudioUnitProperty_AudioUnitMIDIProtocol`),
  events are sent as Universal MIDI Packets through
  `MusicDeviceMIDIEventList`. That symbol is macOS 12+, so it is resolved at
  runtime and the MIDI 1.0 path stays available on older systems.
- Per-note expression (MPE expression messages) has no AUv2 entry point and is
  dropped. MPE still works through per-channel pitch bend and pressure.
- Units receive absolute sample positions through `kAudioUnitProperty_CurrentPlayTime`
  and a lock-free timeline through `kAudioUnitProperty_HostCallbacks`
  (`GetBeatAndTempo`, `GetMusicalTimeLocation`, `GetTransportState` and
  `GetTransportState2`). The callbacks report quarter-note beats (PPQ), BPM,
  the signature in force, the measure downbeat in quarter notes and the frames
  to the next notated beat, so tempo- and meter-synced units follow authored
  `tempo_map` / `time_sig_map` points. Each render call covers one constant
  clock context; the engine splits blocks at every authored change first (see
  [Mapped time seen by plugins](arrangement.md#mapped-time-seen-by-plugins)).
- A unit that rejects the host-callback property still renders: the engine
  records `AU component '…' does not accept host tempo callbacks; its
  tempo-synced internals keep a constant context` instead of failing the
  export. The warning names the component, so a misbehaving unit is
  identifiable from the CLI output.

## Preset state

`kAudioUnitProperty_ClassInfo` is the state channel, which is precisely what an
`.aupreset` file contains. A preset can declare it as:

```ocp
@preset hall
  source: au
  plugin: "Apple: AUReverb2"
  params: { drywetmix: 60, au_state_file: "states/hall.aupreset" }
```

or inline as `au_state_hex`. `plugin_state_file` / `state_file` are accepted as
generic aliases. The file is parsed as a property list (binary or XML) and
applied to the unit; garbage state fails with a typed error and the unit keeps
its default patch. Capturing state back out (`AuInstance::state_bytes`) writes
a binary property list.

## Latency and tail

`kAudioUnitProperty_Latency` becomes the track's source latency, so parallel
paths stay aligned. `kAudioUnitProperty_TailTime` extends the render length the
way VST3 and CLAP tails do, capped at 30 seconds per unit. Units that report
neither (most instruments) add nothing.

## Lifecycle and threading

AudioToolbox keeps process-wide state behind instance creation, initialization,
property writes and teardown. Two threads entering those paths at the same time
abort or corrupt the framework even when they own different components:
concurrent `AudioUnitInitialize` calls trip a framework assertion,
`AudioUnitUninitialize` segfaults, and two `ClassInfo` writes to different
instances of one component fail with `kAudioUnitProperty_ClassInfo`. One
process-wide lock (`AU_API` in `crates/oc-host/src/au_instance.rs`) covers every
entry point, and the contract is:

- **Ownership.** An `AuInstance` belongs to the thread that renders it. The
  value is `Send` so a renderer may move it between workers, but it is never
  shared: every call takes `&mut self`/`&self` from one owner, and `Drop` is the
  only teardown path.
- **Create.** `AuInstance::load` holds the lock from
  `AudioComponentInstanceNew` through `AudioUnitInitialize`, including stream
  formats, the input render callback, host callbacks and parameter discovery.
  Initialization is never split across threads.
- **Use.** Rendering, MIDI, parameters, properties, state and reset all take the
  same lock. AU tracks therefore render one at a time; parallel track rendering
  resumes as soon as the unit call returns. The lock is exclusive because two
  instances of one component are not independent: property writes and teardown
  in one can assert inside the other.
- **Teardown.** `Drop` takes the lock, then, in order: clears the input render
  callback (effects), clears `kAudioUnitProperty_HostCallbacks`, calls
  `AudioUnitUninitialize` when the unit was initialized, and finally
  `AudioComponentInstanceDispose`. Detaching the callbacks first means no
  framework thread can call into the host clock or input buffers once teardown
  starts. A callback that is pending at that point is dropped by the unit while
  it uninitializes; the host never waits for one.
- **The tail probe.** `project_instrument_tail_samples` instantiates a unit
  only to read `kAudioUnitProperty_TailTime`. The probe owns that unit until
  the closure returns and drops it there, under the same lock, so its teardown
  cannot overlap a track render.

The offline renderer is the verified surface. The GUI's live render thread
builds AU instruments and effects through the same `AuInstance` code and
inherits the lock; that path is not covered by the AU stress tests.

Two tests guard the contract: `concurrent_lifecycle_and_render_cycles_stay_consistent`
and `concurrent_state_restore_stays_valid` in `au_instance::tests`. Both spawn
eight workers that create, render, capture/restore `ClassInfo` and drop system
units in tight cycles; before the lock, the first crashed on every run and the
second on most runs.

### Remaining risks

- Apple does not document AUv2 thread-safety, and this lock is our contract,
  not a promise the framework makes: a unit that starts its own thread and
  calls back into the host after `AudioUnitUninitialize` can still outlive the
  instance. Clearing the callback properties narrows that window, but it cannot
  prove the window is closed for every third-party unit.
- Serialising every unit call is conservative. It costs parallel AU rendering,
  so a project with several AU effects renders those units one after another.
  The audio is identical; only the wall-clock time of the FX stage changes.
- The fix was verified against the system units (DLSMusicDevice, AUReverb2,
  AUDynamicsProcessor) and with a multi-thread stress harness. A
  third-party unit with unusual teardown or global render state is not covered
  by that evidence.

## What cannot be hosted in process

These component types are rejected with a typed error that names the type code,
and their tracks keep the scan-only (silent) path with a warning:

| Type | Code | Reason |
|------|------|--------|
| Generator | `augn` | needs host-owned buffers or an output unit (file player, net receive, speech, scheduled player) |
| Mixer | `aumx` | needs a multi-input device graph |
| Panner | `aupn` | needs channel-layout negotiation and a downstream render chain |
| Output | `auou` | drives hardware |
| Format converter | `aufc` | converts formats; no musical output |
| Offline effect | `auol` | not a realtime unit |
| MIDI processor | `aumi` | consumes MIDI without audio |
| Speech synthesizer | `ausp` | not a render unit |

Other limits:

- One stereo output bus. Sends that ask for an auxiliary output pair are an
  error instead of silently reading the main pair twice.
- In-process only: an AUv3 that only runs out of process falls back to the
  warning path rather than being bridged.
- Verified surface is the offline render/export path. The GUI's live render
  thread builds AU instruments and effects through the same instance and
  `BlockSource` code, but the GUI does not audition an AU preset in place yet,
  and that live path is not covered by the checks listed here.

## Failure behaviour

Every `OSStatus` maps to `AuError` with a kind (`NotFound`, `Ambiguous`,
`UnsupportedType`, `InvalidDescription`, `InitFailed`, `PropertyFailed`,
`RenderFailed`, `MidiFailed`, `StateFailed`), the status code (`0x…`) and the
AudioUnit error name when the code is a known `kAudioUnitErr_*`. A unit that
fails to resolve or initialize never aborts the render: the offline renderer
records `AU preset '…' used by track '…' cannot render in process: …; its
tracks render silence`, keeps the rest of the project, and the CLI prints the
warning. `ocp render --report` shows the levels, so the silent track is visible
in the report as well.
