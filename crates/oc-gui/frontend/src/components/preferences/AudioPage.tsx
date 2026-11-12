/**
 * The Audio page: output device and format, capture device and layout,
 * monitoring, and the MIDI input. Opening the page reads the current state; it
 * never opens the capture stream, so no microphone prompt appears.
 */
import { For, Show, onCleanup, onMount } from "solid-js";
import { i18n } from "../../stores/i18n";
import { audioIoStore } from "../../stores/audioIo";
import { midiNoteName, midiStore } from "../../stores/midiLive";
import { recordStore } from "../../stores/record";
import {
  audioFailureText,
  audioInputFailureText,
  audioLatencyValue,
  audioNoticeText,
  audioPendingText,
} from "../../stores/audioIoText";
import { useTooltip } from "../Tooltip";

export function AudioPage() {
  onMount(() => {
    // Enumerate devices and re-read the persisted selections. Neither command
    // opens a stream: `get_audio_io` reports the last measured state.
    void midiStore.openPreferences();
    void audioIoStore.openPreferences();
    // Latency and underruns move while a stream runs; the poll dies with the page.
    const timer = setInterval(() => void audioIoStore.readStatus(), 1000);
    onCleanup(() => clearInterval(timer));
  });

  const audioBusy = () => audioIoStore.pending() !== null;
  const outputLocked = recordStore.recording;
  const inputLocked = () => recordStore.recording() && recordStore.mode() === "audio";
  const audioDevice = () =>
    audioIoStore.devices().find((device) => device.key === audioIoStore.opened()?.key) ?? null;
  const audioRateOptions = (): number[] => {
    const rates = audioDevice()?.sample_rates ?? [];
    const openedRate = audioIoStore.opened()?.sample_rate;
    if (openedRate !== undefined && !rates.includes(openedRate)) return [...rates, openedRate];
    return rates;
  };
  const audioBufferOptions = (): number[] => {
    const range = audioDevice()?.buffer_frames;
    if (!range) return [];
    const ladder = [64, 128, 256, 512, 1024, 2048].filter(
      (frames) => frames >= range[0] && frames <= range[1],
    );
    const current = audioIoStore.opened()?.buffer_frames;
    if (current !== null && current !== undefined && !ladder.includes(current)) ladder.push(current);
    return ladder.sort((a, b) => a - b);
  };
  const audioRateDisabled = () =>
    outputLocked() || audioBusy() || audioIoStore.opened() === null || audioRateOptions().length <= 1;
  const audioBufferDisabled = () =>
    outputLocked() || audioBusy() || audioIoStore.opened() === null || audioDevice()?.buffer_frames == null;
  const audioRateValue = () => {
    const rate = audioIoStore.rateValue();
    return rate === null ? "" : String(rate);
  };
  const audioBufferValue = () => {
    const frames = audioIoStore.bufferValue();
    return frames === null ? "" : String(frames);
  };
  const audioOutputReason = () => {
    if (audioBusy()) return "";
    if (audioIoStore.devices().length === 0) return i18n.t("toolbar.audioNoDevicesReason");
    return "";
  };
  const audioRateReason = () => {
    if (audioBusy()) return "";
    if (audioIoStore.opened() === null) return i18n.t("toolbar.audioOpenFirst");
    if (audioRateOptions().length <= 1) {
      return i18n.t("toolbar.audioOneRate", { rate: audioRateOptions()[0] ?? 0 });
    }
    return "";
  };
  const audioBufferReason = () => {
    if (audioBusy()) return "";
    if (audioIoStore.opened() === null) return i18n.t("toolbar.audioOpenFirst");
    if (audioDevice()?.buffer_frames == null) return i18n.t("toolbar.audioDeviceBuffer");
    return "";
  };
  const audioLatencyText = () => {
    if (!audioIoStore.streamOpen() || audioIoStore.callbackFrames() === 0) return "—";
    return audioLatencyValue(audioIoStore.latencySeconds(), audioIoStore.latencyFrames());
  };
  const audioOpenedLine = () => {
    const opened = audioIoStore.opened();
    if (!opened) return "—";
    const buffer =
      opened.buffer_frames === null
        ? i18n.t("toolbar.audioDeviceDefault")
        : i18n.t("toolbar.audioFrames", { frames: opened.buffer_frames });
    return i18n.t("toolbar.audioOpenedLine", {
      device: opened.name,
      rate: opened.sample_rate,
      buffer,
    });
  };
  const audioMissingText = () => {
    const name = audioIoStore.status().selection.output_device_name ?? "";
    const opened = audioIoStore.opened();
    if (!opened) return i18n.t("toolbar.audioMissingNoFallback");
    return i18n.t("toolbar.audioMissingDevice", { name, fallback: opened.name });
  };
  const audioNotices = () =>
    audioIoStore.status().notices.filter((notice) => notice.kind !== "missing_device");
  const audioFailure = () => {
    const failure = audioIoStore.status().failure;
    if (failure) return audioFailureText(failure);
    const error = audioIoStore.error();
    return error ? i18n.t("toolbar.audioCommandFailed", { error }) : "";
  };

  const inputBusy = () => {
    const pending = audioIoStore.pending();
    return pending?.kind === "input" || pending?.kind === "channels" || pending?.kind === "monitor";
  };
  const inputDevice = () =>
    audioIoStore.inputs().find((device) => device.key === audioIoStore.openedInput()?.key) ?? null;
  const inputDisabledReason = () => {
    if (inputBusy()) return "";
    if (audioIoStore.inputs().length === 0) return i18n.t("toolbar.audioInputNoDevicesReason");
    return "";
  };
  const inputChannelsReason = () => {
    if (inputBusy()) return "";
    if (audioIoStore.inputs().length === 0) return i18n.t("toolbar.audioInputNoDevicesReason");
    if ((inputDevice()?.channels ?? 0) < 2) return i18n.t("toolbar.audioInputChannelsReduced", {
      requested: 2,
      chosen: 1,
    });
    return "";
  };
  const inputMonitorReason = () => {
    if (inputBusy()) return "";
    if (audioIoStore.inputs().length === 0) return i18n.t("toolbar.audioInputNoDevicesReason");
    return "";
  };
  const inputLevelText = () => {
    const view = audioIoStore.inputView();
    if (!view.open) return i18n.t("preferences.audio.notCapturing");
    const peak = audioIoStore.inputLevel();
    return peak <= 0 ? "-∞ dBFS" : `${(20 * Math.log10(peak)).toFixed(1)} dBFS`;
  };
  const inputMissingText = () => {
    const name = audioIoStore.inputStatus().input_device_name ?? "";
    const opened = audioIoStore.openedInput();
    if (!opened) return i18n.t("toolbar.audioInputMissingNoFallback");
    return i18n.t("toolbar.audioInputMissingDevice", { name, fallback: opened.name });
  };
  const inputNotices = () =>
    audioIoStore
      .status()
      .input_notices.filter(
        (notice) => notice.kind !== "missing_input_device" && notice.kind !== "input_failed",
      );
  const inputFailure = () => {
    const failure = audioIoStore.status().input_failure;
    if (failure) return audioInputFailureText(failure);
    const closed = audioIoStore.inputView().error;
    return closed ?? "";
  };

  const midiActivityLabel = () => {
    const current = midiStore.activity();
    if (current.error) return current.error;
    const last = current.last;
    if (!last) return i18n.t("toolbar.midiNoActivity");
    if (last.note !== null) {
      return i18n.t("toolbar.midiLastNote", {
        note: midiNoteName(last.note),
        velocity: last.velocity ?? 0,
      });
    }
    return i18n.t("toolbar.midiLastEvent", { event: last.kind });
  };

  // Capture is opened only by an explicit input choice, monitoring, or an armed
  // take; rendering this page must never touch the microphone. That is a
  // contract for the backend (`list_audio_io` reads the cached view), not a
  // sentence the user needs to read.
  return (
    <>
      <section
        class="audio-io-section"
        classList={{ "audio-io-busy": audioBusy() }}
        aria-label={i18n.t("toolbar.audioIo")}
      >
        <div class="midi-section-header">
          <div class="keymap-title">{i18n.t("toolbar.audioIo")}</div>
          <button
            type="button"
            class="toolbar-btn text-btn midi-refresh audio-io-refresh"
            {...useTooltip(() => i18n.t("toolbar.audioRefresh"))}
            aria-label={i18n.t("toolbar.audioRefresh")}
            disabled={audioBusy()}
            onClick={() => void audioIoStore.refresh()}
          >
            ↻
          </button>
        </div>

        <Show when={audioIoStore.missing()}>
          <p class="audio-io-missing selectable">{audioMissingText()}</p>
        </Show>
        <Show when={audioIoStore.devices().length === 0}>
          <p class="midi-hint">{i18n.t("toolbar.audioNoDevices")}</p>
        </Show>

        <label class="theme-select-label" {...useTooltip(() => i18n.t("toolbar.audioOutputDevice"))}>
          <span>{i18n.t("toolbar.audioOutputDevice")}</span>
          <select
            class="theme-select audio-output-select"
            value={audioIoStore.outputValue()}
            disabled={outputLocked() || audioBusy() || audioIoStore.devices().length === 0}
            onChange={(event) => void audioIoStore.selectOutput(event.currentTarget.value || null)}
          >
            <option value="">{i18n.t("toolbar.audioSystemDefault")}</option>
            <For each={audioIoStore.devices()}>
              {(device) => <option value={device.key}>{device.name}</option>}
            </For>
          </select>
        </label>
        <Show when={audioOutputReason()}>
          <p class="midi-hint">{audioOutputReason()}</p>
        </Show>

        <label class="theme-select-label" {...useTooltip(() => i18n.t("toolbar.audioSampleRate"))}>
          <span>{i18n.t("toolbar.audioSampleRate")}</span>
          <select
            class="theme-select audio-rate-select"
            value={audioRateValue()}
            disabled={audioRateDisabled()}
            onChange={(event) =>
              void audioIoStore.selectRate(
                event.currentTarget.value ? Number(event.currentTarget.value) : null,
              )
            }
          >
            <option value="">{i18n.t("toolbar.audioDeviceDefault")}</option>
            <For each={audioRateOptions()}>
              {(rate) => <option value={String(rate)}>{`${rate} Hz`}</option>}
            </For>
          </select>
        </label>
        <Show when={audioRateReason()}>
          <p class="midi-hint">{audioRateReason()}</p>
        </Show>

        <label class="theme-select-label" {...useTooltip(() => i18n.t("toolbar.audioBufferSize"))}>
          <span>{i18n.t("toolbar.audioBufferSize")}</span>
          <select
            class="theme-select audio-buffer-select"
            value={audioBufferValue()}
            disabled={audioBufferDisabled()}
            onChange={(event) =>
              void audioIoStore.setBuffer(
                event.currentTarget.value ? Number(event.currentTarget.value) : null,
              )
            }
          >
            <option value="">{i18n.t("toolbar.audioDeviceDefault")}</option>
            <For each={audioBufferOptions()}>
              {(frames) => (
                <option value={String(frames)}>
                  {i18n.t("toolbar.audioFrames", { frames })}
                </option>
              )}
            </For>
          </select>
        </label>
        <Show when={audioBufferReason()}>
          <p class="midi-hint">{audioBufferReason()}</p>
        </Show>
        <Show when={outputLocked()}>
          <p class="midi-hint">{i18n.t("toolbar.audioRecordingActive")}</p>
        </Show>

        <div class="audio-io-row">
          <span class="audio-io-label">{i18n.t("toolbar.audioLatency")}</span>
          <span class="audio-io-readout audio-latency">{audioLatencyText()}</span>
        </div>
        <Show when={!audioIoStore.streamOpen()}>
          <p class="midi-hint">{i18n.t("toolbar.audioNoStream")}</p>
        </Show>
        <Show when={audioIoStore.streamOpen() && audioIoStore.callbackFrames() === 0}>
          <p class="midi-hint">{i18n.t("toolbar.audioMeasuring")}</p>
        </Show>

        <Show when={audioIoStore.opened()}>
          <p class="audio-io-readout audio-status-line selectable">{audioOpenedLine()}</p>
        </Show>
        <Show when={audioIoStore.underruns() > 0}>
          <p class="midi-hint">
            {i18n.t("toolbar.audioUnderruns", { count: audioIoStore.underruns() })}
          </p>
        </Show>

        <For each={audioNotices()}>
          {(notice) => <p class="audio-io-notice selectable">{audioNoticeText(notice)}</p>}
        </For>
        <Show when={audioFailure()}>
          {(failure) => <p class="audio-io-failure selectable">{failure()}</p>}
        </Show>
        <Show when={audioIoStore.pending()}>
          {(pending) => <p class="midi-hint audio-io-pending">{audioPendingText(pending())}</p>}
        </Show>

        <div class="preferences-divider" />

        <div class="midi-section-header">
          <div class="keymap-title">{i18n.t("toolbar.audioInputDevice")}</div>
        </div>

        <Show when={audioIoStore.missingInput()}>
          <p class="audio-io-missing selectable">{inputMissingText()}</p>
        </Show>
        <Show when={audioIoStore.inputs().length === 0}>
          <p class="midi-hint">{i18n.t("toolbar.audioInputNoDevices")}</p>
        </Show>

        <label class="theme-select-label" {...useTooltip(() => i18n.t("toolbar.audioInputDevice"))}>
          <span>{i18n.t("toolbar.audioInputDevice")}</span>
          <select
            class="theme-select audio-input-select"
            value={audioIoStore.inputValue()}
            disabled={inputLocked() || audioIoStore.inputs().length === 0}
            onChange={(event) => void audioIoStore.selectInput(event.currentTarget.value || null)}
          >
            <option value="">{i18n.t("toolbar.audioSystemDefault")}</option>
            <For each={audioIoStore.inputs()}>
              {(device) => <option value={device.key}>{device.name}</option>}
            </For>
          </select>
        </label>
        <Show when={inputDisabledReason()}>
          <p class="midi-hint audio-input-reason">{inputDisabledReason()}</p>
        </Show>

        <label class="theme-select-label" {...useTooltip(() => i18n.t("toolbar.audioInputChannels"))}>
          <span>{i18n.t("toolbar.audioInputChannels")}</span>
          <select
            class="theme-select audio-input-channels"
            value={audioIoStore.inputChannels()}
            disabled={inputLocked() || audioIoStore.inputs().length === 0}
            onChange={(event) =>
              void audioIoStore.selectInputChannels(
                event.currentTarget.value === "stereo" ? "stereo" : "mono",
              )
            }
          >
            <option value="mono">{i18n.t("toolbar.audioInputMono")}</option>
            <option value="stereo">{i18n.t("toolbar.audioInputStereo")}</option>
          </select>
        </label>
        <Show when={inputChannelsReason()}>
          <p class="midi-hint audio-input-channels-reason">{inputChannelsReason()}</p>
        </Show>

        <div class="midi-monitor-row">
          <label class="midi-monitor-label">
            <input
              type="checkbox"
              class="audio-monitor-toggle"
              checked={audioIoStore.monitoring()}
              disabled={audioIoStore.inputs().length === 0}
              onChange={(event) => void audioIoStore.setInputMonitor(event.currentTarget.checked)}
            />
            <span>{i18n.t("toolbar.audioMonitorInput")}</span>
          </label>
        </div>
        <Show when={inputMonitorReason()}>
          <p class="midi-hint audio-monitor-reason">{inputMonitorReason()}</p>
        </Show>
        <Show when={audioIoStore.monitoring()}>
          <p class="midi-hint audio-monitor-hint">{i18n.t("toolbar.audioMonitorHint")}</p>
        </Show>

        <div class="preferences-gain-row" {...useTooltip(() => i18n.t("preferences.audio.monitorGain"))}>
          <span class="preferences-gain-label">{i18n.t("preferences.audio.monitorGain")}</span>
          <input
            type="range"
            class="preferences-range audio-monitor-gain"
            aria-label={i18n.t("preferences.audio.monitorGain")}
            min="-24"
            max="12"
            step="0.5"
            value={audioIoStore.monitorGainDb()}
            disabled={!audioIoStore.monitoring() || audioIoStore.inputs().length === 0}
            onChange={(event) => void audioIoStore.setInputMonitorGain(Number(event.currentTarget.value))}
          />
          <span class="preferences-inline-value preferences-gain-value">
            {i18n.t("preferences.audio.gainValue", {
              db: audioIoStore.monitorGainDb().toFixed(1),
            })}
          </span>
        </div>

        <div class="audio-io-row">
          <span class="audio-io-label">{i18n.t("toolbar.audioInputLevel")}</span>
          <span class="audio-io-readout audio-input-level">{inputLevelText()}</span>
          <Show when={audioIoStore.inputClipped()}>
            <span class="audio-input-clip">{i18n.t("toolbar.audioInputClip")}</span>
          </Show>
        </div>
        <Show when={!audioIoStore.inputView().open}>
          <p class="midi-hint audio-input-closed">{i18n.t("toolbar.audioInputClosed")}</p>
        </Show>

        <For each={inputNotices()}>
          {(notice) => <p class="audio-io-notice selectable">{audioNoticeText(notice)}</p>}
        </For>
        <Show when={inputFailure()}>
          {(failure) => <p class="audio-io-failure selectable">{failure()}</p>}
        </Show>
        <Show when={audioIoStore.inputView().monitor_error}>
          {(error) => (
            <p class="audio-io-failure selectable">
              {i18n.t("toolbar.audioMonitorOutputFailed", { error: error() })}
            </p>
          )}
        </Show>
      </section>

      <div class="preferences-divider" />

      <section class="midi-live-section" aria-label={i18n.t("toolbar.midiInput")}>
        <div class="midi-section-header">
          <div class="keymap-title">{i18n.t("toolbar.midiInput")}</div>
          <button
            type="button"
            class="toolbar-btn text-btn midi-refresh"
            {...useTooltip(() => i18n.t("toolbar.midiRefresh"))}
            aria-label={i18n.t("toolbar.midiRefresh")}
            onClick={() => void midiStore.refreshPorts()}
          >
            ↻
          </button>
        </div>
        <Show
          when={midiStore.ports().supported}
          fallback={
            <p class="midi-hint midi-unavailable selectable">
              {midiStore.ports().error ?? i18n.t("toolbar.midiUnavailable")}
            </p>
          }
        >
          <Show
            when={midiStore.ports().inputs.length > 0 || midiStore.missingSelection()}
            fallback={<p class="midi-hint">{i18n.t("toolbar.midiNoDevices")}</p>}
          >
            <label class="theme-select-label" {...useTooltip(() => i18n.t("toolbar.midiInput"))}>
              <span>{i18n.t("toolbar.midiInput")}</span>
              <select
                class="theme-select midi-input-select"
                value={midiStore.config().input ?? ""}
                disabled={midiStore.busy()}
                onChange={(event) => void midiStore.selectInput(event.currentTarget.value || null)}
              >
                <option value="">{i18n.t("toolbar.midiNone")}</option>
                <For each={midiStore.ports().inputs}>
                  {(port) => <option value={port.name}>{port.name}</option>}
                </For>
                <Show when={midiStore.missingSelection()}>
                  <option value={midiStore.config().input ?? ""}>
                    {i18n.t("toolbar.midiUnavailableDevice", {
                      name: midiStore.config().input ?? "",
                    })}
                  </option>
                </Show>
              </select>
            </label>
          </Show>
          <div class="midi-monitor-row">
            <label class="midi-monitor-label">
              <input
                type="checkbox"
                class="midi-monitor-toggle"
                checked={midiStore.config().monitor}
                disabled={midiStore.busy() || !midiStore.config().input}
                onChange={(event) => void midiStore.setMonitor(event.currentTarget.checked)}
              />
              <span>{i18n.t("toolbar.midiMonitor")}</span>
            </label>
          </div>
          <Show when={!midiStore.config().input}>
            <p class="midi-hint midi-monitor-hint">{i18n.t("toolbar.midiMonitorNeedsInput")}</p>
          </Show>
          <Show when={midiStore.config().error}>
            <p class="midi-error selectable">{midiStore.config().error}</p>
          </Show>
          <Show
            when={
              midiStore.config().monitor &&
              midiStore.config().active &&
              !midiStore.auditionActive()
            }
          >
            <p class="midi-hint">{i18n.t("toolbar.midiAuditionHint")}</p>
          </Show>
          <Show when={midiStore.config().monitor || midiStore.activity().events > 0}>
            <p class="midi-activity">{midiActivityLabel()}</p>
          </Show>
        </Show>
      </section>
    </>
  );
}
