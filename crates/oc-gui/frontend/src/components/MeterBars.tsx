/**
 * Mixer meters: peak/RMS bars per channel, the clip hold, and the master's
 * loudness, correlation, goniometer and ten-second level history.
 *
 * Every reading comes from the engine; a stopped transport or a silent tap
 * shows a note and a dash, never a frozen or invented level.
 */
import { For, Show, createSignal, onCleanup, onMount } from "solid-js";
import { i18n } from "../stores/i18n";
import {
  masterHistory,
  meterSnapshot,
  resetMeterClip,
  selectChannelMeter,
  selectMasterMonitor,
  setMonitorMono,
  startMeterPolling,
} from "../stores/meters";
import { METER_HISTORY_CAPACITY, METER_HISTORY_SECONDS, pointsAttribute, traceSegments } from "../utils/meterHistory";
import { formatCorrelation, formatLevelDb, formatLufs, levelPercent } from "../utils/meterFormat";

/** Why a channel has no reading, or null while it is measuring. */
function meterNote(
  row: { reason: string | null } | null,
  available: boolean,
  playing: boolean,
): string | null {
  if (!available) {
    return row?.reason
      ? i18n.t("mixer.meter.unavailable", { reason: row.reason })
      : i18n.t("mixer.meter.needsPlayback");
  }
  if (!row || !playing) return i18n.t("mixer.meter.needsPlayback");
  return null;
}

/** The note for a surface with no snapshot at all: the poll itself failed. */
function noSnapshotNote(): string {
  return i18n.t("mixer.meter.unavailable", { reason: i18n.t("mixer.meter.noEngine") });
}

/** Peak and RMS bars, the readout and the clip hold for one channel. */
export function ChannelMeter(props: { target: string; note?: boolean }) {
  onMount(() => {
    const release = startMeterPolling();
    onCleanup(release);
  });

  const row = () => selectChannelMeter(meterSnapshot(), props.target);
  const note = () => {
    const snapshot = meterSnapshot();
    if (!snapshot) return noSnapshotNote();
    return meterNote(row(), snapshot.available, snapshot.playing);
  };
  const live = () => note() === null;
  const peak = () => (live() ? formatLevelDb(row()?.peakDb ?? null) : i18n.t("mixer.meter.noReading"));
  const rms = () => (live() ? formatLevelDb(row()?.rmsDb ?? null) : i18n.t("mixer.meter.noReading"));
  const clipped = () => live() && row()?.clipped === true;

  return (
    <div
      class="channel-meter"
      data-channel-meter={props.target}
      data-meter-live={live() ? "true" : "false"}
      data-meter-clipped={clipped() ? "true" : "false"}
    >
      <div class="channel-meter-bars" aria-hidden="true">
        <span
          class="channel-meter-bar channel-meter-peak"
          data-meter-peak-bar
          style={{ width: `${live() ? levelPercent(row()?.peakDb ?? null) : 0}%` }}
        />
        <span
          class="channel-meter-bar channel-meter-rms"
          data-meter-rms-bar
          style={{ width: `${live() ? levelPercent(row()?.rmsDb ?? null) : 0}%` }}
        />
      </div>
      <div class="channel-meter-readout">
        <span data-meter-peak>{i18n.t("mixer.meter.peak", { value: peak() })}</span>
        <span data-meter-rms>{i18n.t("mixer.meter.rms", { value: rms() })}</span>
      </div>
      <div class="channel-meter-actions">
        <button
          class="track-btn channel-meter-clip"
          classList={{ "clip-held": clipped() }}
          data-meter-clip
          aria-pressed={clipped()}
          aria-label={
            clipped()
              ? i18n.t("mixer.meter.clipHeld")
              : i18n.t("mixer.meter.resetClip")
          }
          title={i18n.t("mixer.meter.resetClip")}
          onClick={(event) => {
            event.stopPropagation();
            void resetMeterClip();
          }}
        >{i18n.t("mixer.meter.clip")}</button>
      </div>
      <Show when={props.note === false ? null : note()}>
        {(text) => (
          <div class="channel-meter-note" data-meter-note role="note">
            {text()}
          </div>
        )}
      </Show>
    </div>
  );
}

/** The master's LUFS-S readout, correlation, goniometer, mono check and history. */
export function MasterMonitor() {
  onMount(() => {
    const release = startMeterPolling();
    onCleanup(release);
  });

  const [monoError, setMonoError] = createSignal<string | null>(null);
  const [monoBusy, setMonoBusy] = createSignal(false);
  const monitor = () => selectMasterMonitor(meterSnapshot());
  const note = () => {
    const snapshot = meterSnapshot();
    if (!snapshot) return noSnapshotNote();
    return meterNote(monitor()?.meter ?? null, snapshot.available, snapshot.playing);
  };
  const live = () => note() === null;
  const lufs = () => formatLufs(monitor()?.lufs ?? null);
  const lufsNote = () => {
    if (!live()) return null;
    if ((monitor()?.lufs ?? null) !== null) return null;
    return i18n.t("mixer.lufsFilling", { seconds: (monitor()?.windowSeconds ?? 3).toFixed(0) });
  };
  const correlationText = () =>
    live() ? formatCorrelation(monitor()?.correlation ?? null) : i18n.t("mixer.meter.noReading");
  const goniometerPoints = () => {
    const values = monitor()?.goniometer ?? [];
    const pairs: { x: number; y: number }[] = [];
    for (let index = 0; index + 1 < values.length; index += 2) {
      const left = values[index];
      const right = values[index + 1];
      // Rotate the pair 45°: side on x, mid on y, as a vectorscope reads.
      const x = (left - right) / 2;
      const y = (left + right) / 2;
      pairs.push({
        x: Math.min(1, Math.max(-1, x)),
        y: Math.min(1, Math.max(-1, y)),
      });
    }
    return pairs;
  };
  const canMono = () => {
    const row = monitor();
    return row !== null && row.masterChannels === 2 && row.playing;
  };
  const monoReason = () => {
    const row = monitor();
    if (!row) return i18n.t("mixer.monoNeedsStereo");
    if (row.masterChannels !== 2) return i18n.t("mixer.monoNeedsStereo");
    if (!row.playing) return i18n.t("mixer.monoNeedsPlayback");
    return null;
  };
  const monoNote = () => {
    const failure = monoError();
    if (failure) return i18n.t("mixer.monoFailed", { reason: failure });
    return monoReason();
  };
  const monoLabel = () => {
    const row = monitor();
    if (monoBusy() || (row?.monoPending && row.playing)) {
      return i18n.t("mixer.monoApplying");
    }
    return i18n.t("mixer.mono");
  };
  const segments = () => traceSegments(masterHistory(), 100, 24);

  const toggleMono = async () => {
    const row = monitor();
    if (!row) return;
    setMonoBusy(true);
    setMonoError(null);
    try {
      await setMonitorMono(!row.mono.requested);
    } catch (error) {
      setMonoError(error instanceof Error ? error.message : String(error));
    } finally {
      setMonoBusy(false);
    }
  };

  return (
    <div class="master-monitor" data-master-monitor>
      <div class="master-monitor-head">
        <span class="channel-strip-section">{i18n.t("mixer.loudness")}</span>
        <span
          class="master-monitor-value"
          data-master-lufs
          data-lufs-window={String(monitor()?.windowSeconds ?? 3)}
        >{live() ? i18n.t("mixer.lufs", { value: lufs() }) : i18n.t("mixer.meter.noReading")}</span>
      </div>
      <Show when={lufsNote()}>
        {(text) => <div class="channel-meter-note" data-lufs-note role="note">{text()}</div>}
      </Show>
      <Show when={note()}>
        {(text) => (
          <div class="channel-meter-note" data-monitor-note role="note">
            {text()}
          </div>
        )}
      </Show>

      <div class="master-monitor-head">
        <span class="channel-strip-section">{i18n.t("mixer.goniometer")}</span>
        <span class="master-monitor-value" data-master-correlation>
          {i18n.t("mixer.correlation", { value: correlationText() })}
        </span>
      </div>
      <svg
        class="master-goniometer"
        data-goniometer
        data-goniometer-points={String(goniometerPoints().length)}
        viewBox="-1.1 -1.1 2.2 2.2"
        role="img"
        aria-label={i18n.t("mixer.goniometer")}
      >
        <line class="master-goniometer-frame" x1="-1" y1="0" x2="1" y2="0" />
        <line class="master-goniometer-frame" x1="0" y1="-1" x2="0" y2="1" />
        <line class="master-goniometer-frame" x1="-0.7" y1="-0.7" x2="0.7" y2="0.7" />
        <For each={goniometerPoints()}>
          {(point) => <circle class="master-goniometer-dot" cx={point.x.toFixed(3)} cy={point.y.toFixed(3)} r="0.035" />}
        </For>
      </svg>

      <div class="master-monitor-actions">
        <button
          class="track-btn master-mono"
          data-mono-button
          classList={{ "mono-active": monitor()?.mono.requested === true }}
          aria-pressed={monitor()?.mono.requested === true}
          aria-label={i18n.t("mixer.mono")}
          disabled={!canMono()}
          title={monoReason() ?? i18n.t("mixer.mono")}
          onClick={() => void toggleMono()}
        >{monoLabel()}</button>
      </div>
      <Show when={monoNote()}>
        {(text) => (
          <div class="channel-meter-note" data-mono-note role="note">
            {text()}
          </div>
        )}
      </Show>

      <div class="master-monitor-head">
        <span class="channel-strip-section">{i18n.t("mixer.history")}</span>
        <span class="master-monitor-value" data-history-window>
          {i18n.t("mixer.historyWindow", { seconds: METER_HISTORY_SECONDS })}
        </span>
      </div>
      <svg
        class="master-history"
        data-master-history
        data-history-samples={String(masterHistory().length)}
        data-history-capacity={String(METER_HISTORY_CAPACITY)}
        viewBox="0 0 100 24"
        preserveAspectRatio="none"
        role="img"
        aria-label={i18n.t("mixer.history")}
      >
        <line class="master-history-floor" x1="0" y1="24" x2="100" y2="24" />
        <For each={segments()}>
          {(points) => (
            <polyline class="master-history-trace" points={pointsAttribute(points)} />
          )}
        </For>
      </svg>
    </div>
  );
}
