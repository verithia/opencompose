/**
 * ChannelStrips — horizontal mixer row: track channels, bus returns, master.
 * Sends are edited in place; every edit is one undoable project mutation.
 */
import { For, Show, createSignal, onMount } from "solid-js";
import { store } from "../stores/project";
import { i18n } from "../stores/i18n";
import type { Bus, PluginPortInfo, Track } from "../types/project";
import { ChannelMeter, MasterMonitor } from "./MeterBars";

// Fallback when the theme tokens can't be read (tests, SSR). The live colour is
// the theme's `--track-color-N`, the same data colour the timeline lanes use.
const TRACK_COLORS = ["#e2a842", "#4aa3df", "#8bc34a", "#e06c75", "#b07cd6", "#38b2ac", "#5cb885", "#c77dba"];

function themeTrackColor(index: number): string {
  const slot = index % 8;
  if (typeof document !== "undefined") {
    const value = getComputedStyle(document.documentElement).getPropertyValue(`--track-color-${slot}`).trim();
    if (/^#[0-9a-f]{6}$/i.test(value)) return value;
  }
  return TRACK_COLORS[slot];
}

function amountToDb(amount: number): number {
  return amount <= 0 ? -Infinity : 20 * Math.log10(amount);
}

function dbToAmount(db: number): number {
  return Math.max(0, Math.min(4, 10 ** (db / 20)));
}

/** A blank or malformed numeric field is a no-op, never a zero. */
function fieldNumber(event: { currentTarget: HTMLInputElement }): number | null {
  const text = event.currentTarget.value.trim();
  if (!text) return null;
  const value = Number(text);
  return Number.isFinite(value) ? value : null;
}

function channelColor(track: Track, index: number): string {
  return track.mix.overrides?.color || themeTrackColor(index);
}

function channelLabel(track: Track): string {
  return track.mix.overrides?.display_name || track.name;
}

function SendRow(props: {
  trackIndex: number;
  target: string;
  amount: number;
  preFader: boolean;
  pan: number;
  auxSource?: string;
}) {
  const db = () => amountToDb(props.amount);
  const sendLevelLabel = () => props.auxSource
    ? i18n.t("mixer.sendLevelAux", { source: props.auxSource, target: props.target })
    : i18n.t("mixer.sendLevel", { target: props.target });
  const sendPanLabel = () => props.auxSource
    ? i18n.t("mixer.sendPanAux", { source: props.auxSource, target: props.target })
    : i18n.t("mixer.sendPan", { target: props.target });
  const removeSendLabel = () => props.auxSource
    ? i18n.t("mixer.removeSendAux", { source: props.auxSource, target: props.target })
    : i18n.t("mixer.removeSend", { target: props.target });
  const source = () => {
    if (!props.auxSource) return undefined;
    const [owner, output] = props.auxSource.split(".out");
    return { owner, output: Number(output) };
  };
  return (
    <div class="channel-send" data-send={props.target}>
      <span class="channel-send-target" title={props.auxSource ?? props.target}>
        {props.auxSource ? `${props.auxSource}→` : ""}{props.target}
      </span>
      <input
        aria-label={sendLevelLabel()}
        type="number"
        step="0.5"
        min="-60"
        max="12"
        value={Number.isFinite(db()) ? db().toFixed(1) : "-60"}
        onChange={(e) => {
          const next = fieldNumber(e);
          if (next !== null) {
            void store.setSendAmount(props.trackIndex, props.target, dbToAmount(next), source());
          }
        }}
      />
      <input
        aria-label={sendPanLabel()}
        type="number"
        step="0.05"
        min="-1"
        max="1"
        value={props.pan.toFixed(2)}
        onChange={(e) => {
          const pan = fieldNumber(e);
          if (pan !== null) {
            void store.setSendPan(props.trackIndex, props.target, pan, source());
          }
        }}
      />
      <Show
        when={props.auxSource}
        fallback={
          <button
            class="track-btn channel-send-tap"
            classList={{ "tap-pre": props.preFader }}
            aria-label={i18n.t("mixer.sendPreFader", { target: props.target })}
            aria-pressed={props.preFader}
            onClick={() => void store.setSendPreFader(
              props.trackIndex, props.target, !props.preFader,
            )}
          >{props.preFader ? "PRE" : "POST"}</button>
        }
      >
        <span class="channel-send-tap aux" title={i18n.t("routing.auxHint")}>AUX</span>
      </Show>
      <button
        class="track-btn channel-send-remove"
        aria-label={removeSendLabel()}
        onClick={() => void store.removeSend(props.trackIndex, props.target, source())}
      >×</button>
    </div>
  );
}

function TrackStrip(props: {
  track: Track;
  index: number;
  selected: boolean;
  onSelect: () => void;
  /** Rendered inside the mixer inspector, next to NodeMixer's own M/S. */
  inspector?: boolean;
}) {
  const [sendTarget, setSendTarget] = createSignal("");
  const [sourceOutput, setSourceOutput] = createSignal(1);
  const [auxPairs, setAuxPairs] = createSignal(1);
  const [auxLoading, setAuxLoading] = createSignal(false);
  const [outputPorts, setOutputPorts] = createSignal<PluginPortInfo[]>([]);
  const soloSafe = () => props.track.mix.overrides?.solo_safe ?? false;
  const polarity = () => props.track.mix.polarity_inverted ?? false;
  const presetName = () => props.track.preset;
  const pluginPreset = () => presetName()
    ? store.project.presets.find((preset) => preset.name === presetName()
      && (preset.source === "clap" || preset.source === "vst3" || preset.source === "vst2"))
    : undefined;
  const sendSource = () => sourceOutput() > 1 && presetName()
    ? { owner: presetName()!, output: sourceOutput() }
    : undefined;
  const availableBuses = () => store.project.buses.filter(
    (bus) => !props.track.mix.sends.some((send) => send.target === bus.name
      && (sourceOutput() > 1
        ? send.source?.owner === presetName() && send.source?.output === sourceOutput()
        : !send.source)),
  );
  const loadAuxOutputs = async () => {
    if (!presetName() || !pluginPreset()) return;
    setAuxLoading(true);
    try {
      setAuxPairs(await store.presetOutputPairs(presetName()!));
      // Loading the plugin also reveals its declared output ports.
      setOutputPorts(await store.presetOutputPorts(presetName()!));
    } finally {
      setAuxLoading(false);
    }
  };
  // An already-loaded instrument reports its ports without a second load.
  onMount(async () => {
    if (!presetName() || !pluginPreset()) return;
    setOutputPorts(await store.presetOutputPorts(presetName()!));
  });

  return (
    <div
      class="channel-strip"
      data-channel-strip={`track:${props.index}`}
      title={channelLabel(props.track)}
      classList={{ selected: props.selected, muted: props.track.mix.mute }}
      style={{ "--strip-color": props.track.mix.overrides?.color || `var(--track-color-${props.index % 8})` }}
      onClick={props.onSelect}
    >
      <div class="channel-strip-header">
        <input
          class="channel-color"
          aria-label={i18n.t("mixer.channelColour", { name: props.track.name })}
          type="color"
          value={channelColor(props.track, props.index)}
          onChange={(e) => void store.setTrackChannelOverrides(props.index, {
            color: e.currentTarget.value,
          })}
        />
        <input
          class="channel-name"
          aria-label={i18n.t("mixer.channelName", { name: props.track.name })}
          type="text"
          placeholder={props.track.name}
          // The track has one name: the strip edits it, so the timeline, clips
          // and routing graph agree. A legacy channel label is still shown
          // until the operator renames through here.
          value={props.track.mix.overrides?.display_name || props.track.name}
          onChange={(e) => {
            const next = e.currentTarget.value.trim();
            if (next && next !== props.track.name) void store.renameTrack(props.index, next);
          }}
        />
      </div>

      <div class="channel-strip-buttons">
        <Show when={!props.inspector}>
        <button
          class="track-btn"
          classList={{ "mute-active": props.track.mix.mute }}
          aria-label={props.track.mix.mute ? i18n.t("menu.track.unmute") : i18n.t("menu.track.mute")}
          onClick={(e) => { e.stopPropagation(); void store.toggleMute(props.index); }}
        >M</button>
        <button
          class="track-btn"
          classList={{ "solo-active": props.track.mix.solo }}
          aria-label={props.track.mix.solo ? i18n.t("menu.track.unsolo") : i18n.t("menu.track.solo")}
          onClick={(e) => { e.stopPropagation(); void store.toggleSolo(props.index); }}
        >S</button>
        </Show>
        <button
          class="track-btn"
          classList={{ "solo-safe-active": soloSafe() }}
          aria-label={i18n.t("mixer.soloSafe", { name: props.track.name })}
          aria-pressed={soloSafe()}
          onClick={(e) => {
            e.stopPropagation();
            void store.setTrackChannelOverrides(props.index, { soloSafe: !soloSafe() });
          }}
        >SAFE</button>
        <button
          class="track-btn"
          classList={{ "polarity-active": polarity() }}
          aria-label={i18n.t("mixer.polarity", { name: props.track.name })}
          aria-pressed={polarity()}
          onClick={(e) => {
            e.stopPropagation();
            void store.setTrackChannelOverrides(props.index, { polarityInverted: !polarity() });
          }}
        >Ø</button>
      </div>

      <div class="channel-strip-fader">
        <label>
          <span>{i18n.t("mixer.fader")}</span>
          <input
            aria-label={i18n.t("mixer.faderLabel", { name: props.track.name })}
            type="number"
            step="0.5"
            min="-60"
            max="12"
            value={props.track.mix.volume_db.toFixed(1)}
            onChange={(e) => {
              const db = fieldNumber(e);
              if (db !== null) void store.updateTrackMix(props.index, db, undefined);
            }}
          />
        </label>
        <label>
          <span>{i18n.t("routing.pan")}</span>
          <input
            aria-label={i18n.t("mixer.panLabel", { name: props.track.name })}
            type="number"
            step="0.05"
            min="-1"
            max="1"
            value={props.track.mix.pan.toFixed(2)}
            onChange={(e) => {
              const pan = fieldNumber(e);
              if (pan !== null) void store.updateTrackMix(props.index, undefined, pan);
            }}
          />
        </label>
        <Show when={outputPorts().length > 1}>
          <label class="channel-strip-port">
            <span>{i18n.t("mixer.port")}</span>
            <select
              aria-label={i18n.t("mixer.portLabel", { name: props.track.name })}
              value={String(props.track.output_port ?? 0)}
              onChange={(e) => void store.setTrackOutputPort(props.index, Number(e.currentTarget.value))}
              onClick={(e) => e.stopPropagation()}
            >
              <For each={outputPorts()}>
                {(port, index) => (
                  <option value={String(index())}>
                    {index() + 1} ({port.channels}ch)
                  </option>
                )}
              </For>
            </select>
          </label>
        </Show>
      </div>

      <ChannelMeter target={`track:${props.index}`} />

      <div class="channel-strip-sends">
        <div class="channel-strip-section">{i18n.t("mixer.sends")}</div>
        <For each={props.track.mix.sends}>
          {(send) => (
            <SendRow
              trackIndex={props.index}
              target={send.target}
              amount={send.amount}
              preFader={send.pre_fader ?? false}
              pan={send.pan ?? 0}
              auxSource={send.source ? `${send.source.owner}.out${send.source.output}` : undefined}
            />
          )}
        </For>
        <Show
          when={availableBuses().length > 0 && (!props.inspector || pluginPreset())}
          fallback={<Show when={!props.inspector}><div class="channel-strip-hint">{i18n.t("mixer.noBus")}</div></Show>}
        >
          <div class="channel-send-add">
            <select
              aria-label={i18n.t("mixer.addSendTarget", { name: props.track.name })}
              value={sendTarget()}
              onChange={(e) => setSendTarget(e.currentTarget.value)}
              onClick={(e) => e.stopPropagation()}
            >
              <option value="">{i18n.t("mixer.busPlaceholder")}</option>
              <For each={availableBuses()}>{(bus) => <option value={bus.name}>{bus.name}</option>}</For>
            </select>
            <select
              aria-label={i18n.t("mixer.sendSource", { name: props.track.name })}
              value={String(sourceOutput())}
              onChange={(e) => setSourceOutput(Number(e.currentTarget.value))}
              onClick={(e) => e.stopPropagation()}
              disabled={!pluginPreset()}
              title={pluginPreset() ? undefined : i18n.t("mixer.sendSourceNeedsPlugin")}
            >
              <option value="1">{i18n.t("routing.kindMain")}</option>
              <For each={Array.from({ length: Math.max(0, auxPairs() - 1) }, (_, i) => i + 2)}>
                {(output) => <option value={String(output)}>out{output}</option>}
              </For>
            </select>
            <Show when={pluginPreset()}>
              <button
                class="track-btn channel-send-aux"
                aria-label={i18n.t("mixer.loadAux", { name: props.track.name })}
                disabled={auxLoading()}
                onClick={(e) => { e.stopPropagation(); void loadAuxOutputs(); }}
              >{auxLoading() ? "…" : auxPairs() > 1 ? `${auxPairs() - 1} AUX` : "AUX"}</button>
            </Show>
            <button
              class="track-btn"
              aria-label={i18n.t("mixer.addSendFrom", { name: props.track.name })}
              disabled={!sendTarget()}
              title={sendTarget() ? undefined : i18n.t("mixer.pickTargetFirst")}
              onClick={async (e) => {
                e.stopPropagation();
                if (!sendTarget()) return;
                await store.addSend(props.index, sendTarget(), 0.25, false, sendSource());
                setSendTarget("");
                setSourceOutput(1);
              }}
            >+</button>
          </div>
        </Show>
      </div>
    </div>
  );
}

function BusStrip(props: {
  bus: Bus;
  selected: boolean;
  onSelect: () => void;
}) {
  const [routeTarget, setRouteTarget] = createSignal("");
  const returns = () => props.bus.routes.length === 0 ? ["master"] : props.bus.routes;
  const availableTargets = () => [
    "master",
    ...store.project.buses
      .map((bus) => bus.name)
      .filter((name) => name !== props.bus.name),
  ].filter((name) => !returns().includes(name));

  return (
    <div
      class="channel-strip bus-strip"
      data-channel-strip={`bus:${props.bus.name}`}
      classList={{
        selected: props.selected,
        muted: props.bus.mute ?? false,
        soloed: props.bus.solo ?? false,
      }}
      onClick={props.onSelect}
    >
      <div class="channel-strip-header">
        <span class="channel-badge">{i18n.t("mixer.busBadge")}</span>
        <span class="channel-name static">{props.bus.name}</span>
      </div>
      <div class="channel-strip-buttons">
        <button
          class="track-btn"
          classList={{ "mute-active": props.bus.mute ?? false }}
          aria-label={i18n.t("mixer.muteBus", { name: props.bus.name })}
          aria-pressed={props.bus.mute ?? false}
          onClick={(e) => {
            e.stopPropagation();
            void store.updateBusMix(props.bus.name, undefined, !(props.bus.mute ?? false));
          }}
        >M</button>
        <button
          class="track-btn"
          classList={{ "solo-active": props.bus.solo ?? false }}
          aria-label={i18n.t("mixer.soloBus", { name: props.bus.name })}
          aria-pressed={props.bus.solo ?? false}
          onClick={(e) => {
            e.stopPropagation();
            void store.updateBusMix(props.bus.name, undefined, undefined, !(props.bus.solo ?? false));
          }}
        >S</button>
      </div>
      <div class="channel-strip-fader">
        <label>
          <span>{i18n.t("mixer.fader")}</span>
          <input
            aria-label={i18n.t("mixer.busFader", { name: props.bus.name })}
            type="number"
            step="0.5"
            min="-60"
            max="12"
            value={(props.bus.volume_db ?? 0).toFixed(1)}
            onChange={(e) => {
              const db = fieldNumber(e);
              if (db !== null) void store.updateBusMix(props.bus.name, db);
            }}
          />
        </label>
      </div>
      <ChannelMeter target={`bus:${props.bus.name}`} />
      <div class="channel-strip-section">{i18n.t("mixer.return")}</div>
      <For each={returns()}>
        {(target) => <div class="bus-return">→ {target}</div>}
      </For>
      <div class="channel-strip-hint">{props.bus.effects.length} fx</div>
      <Show when={availableTargets().length > 0}>
        <div class="channel-send-add">
          <select
            aria-label={i18n.t("mixer.addRouteTarget", { name: props.bus.name })}
            value={routeTarget()}
            onChange={(e) => setRouteTarget(e.currentTarget.value)}
            onClick={(e) => e.stopPropagation()}
          >
            <option value="">{i18n.t("mixer.routePlaceholder")}</option>
            <For each={availableTargets()}>{(target) => <option value={target}>{target}</option>}</For>
          </select>
          <button
            class="track-btn"
            aria-label={i18n.t("mixer.addRouteFrom", { name: props.bus.name })}
            disabled={!routeTarget()}
            title={routeTarget() ? undefined : i18n.t("mixer.pickTargetFirst")}
            onClick={async (e) => {
              e.stopPropagation();
              if (!routeTarget()) return;
              await store.setBusRoutes(props.bus.name, [...props.bus.routes, routeTarget()]);
              setRouteTarget("");
            }}
          >+</button>
        </div>
      </Show>
    </div>
  );
}

function MasterStrip(props: { selected: boolean; onSelect: () => void }) {
  const volumeDb = () => store.project.master?.volume_db ?? 0;
  return (
    <div
      class="channel-strip master-strip"
      data-channel-strip="master"
      classList={{ selected: props.selected }}
      onClick={props.onSelect}
    >
      <div class="channel-strip-header">
        <span class="channel-badge">{i18n.t("mixer.masterBadge")}</span>
      </div>
      <div class="channel-strip-fader">
        <label>
          <span>{i18n.t("mixer.fader")}</span>
          <input
            aria-label={i18n.t("mixer.masterFader")}
            type="number"
            step="0.5"
            min="-60"
            max="12"
            value={volumeDb().toFixed(1)}
            onChange={(e) => {
              const db = fieldNumber(e);
              if (db !== null) void store.updateMasterVolume(db);
            }}
          />
        </label>
      </div>
      <ChannelMeter target="master" note={false} />
      <MasterMonitor />
      <div class="channel-strip-section">{i18n.t("device.meter.output")}</div>
      <div class="bus-return">{i18n.t("mixer.outDestination")}</div>
      <div class="channel-strip-hint">{store.project.master?.effects.length ?? 0} fx</div>
    </div>
  );
}

/** The selected channel's strip, shown in the mixer inspector: channel name,
 * colour, solo-safe, polarity, exact fader/pan entry and per-send dB/pan/tap. */
export function ChannelDetail(props: { nodeId: string }) {
  const trackIndex = () => props.nodeId.startsWith("track:") ? Number(props.nodeId.slice(6)) : null;
  const bus = () => props.nodeId.startsWith("bus:")
    ? store.project.buses.find((b) => b.name === props.nodeId.slice(4))
    : undefined;
  return (
    <div class="channel-detail">
      <Show when={trackIndex() !== null && store.project.tracks[trackIndex()!]}>
        {(track) => (
          <TrackStrip track={track()} index={trackIndex()!} selected={false} onSelect={() => {}} inspector />
        )}
      </Show>
      <Show when={bus()}>
        {(b) => <BusStrip bus={b()} selected={false} onSelect={() => {}} />}
      </Show>
      <Show when={props.nodeId === "master"}>
        <MasterStrip selected={false} onSelect={() => {}} />
      </Show>
    </div>
  );
}

export function ChannelStrips(props: {
  selectedNode: string | null;
  onSelectNode: (id: string) => void;
}) {
  return (
    <div class="mixer-channel-strips" role="group" aria-label={i18n.t("mixer.channelStrips")}>
      <For each={store.project.tracks}>
        {(track, index) => (
          <TrackStrip
            track={track}
            index={index()}
            selected={props.selectedNode === `track:${index()}`}
            onSelect={() => props.onSelectNode(`track:${index()}`)}
          />
        )}
      </For>
      <For each={store.project.buses}>
        {(bus) => (
          <BusStrip
            bus={bus}
            selected={props.selectedNode === `bus:${bus.name}`}
            onSelect={() => props.onSelectNode(`bus:${bus.name}`)}
          />
        )}
      </For>
      <MasterStrip
        selected={props.selectedNode === "master"}
        onSelect={() => props.onSelectNode("master")}
      />
    </div>
  );
}
