import { AUTOMATION_CURVES } from "../data/automationCurves.gen";
import { For, Show, createMemo, createSignal, createUniqueId } from "solid-js";
import type { GainAutomation } from "../types/project";
import type { TimelineViewport } from "../utils/timelineViewport";
import {
  gainAutomationView,
  gainAutomationDomain,
} from "../utils/gainAutomationView";
import {
  lanePolyline,
  mappedAutomationValue,
  pointValue,
  pointXY,
  AUTOMATION_ROW_HEIGHT,
} from "../utils/automationRow";
import { projectClock } from "../utils/projectClock";
import { store } from "../stores/project";
import { i18n } from "../stores/i18n";
import { gainAutomationUi } from "../stores/gainAutomationUi";

export function GainAutomationPreview(props: {
  curve: GainAutomation;
  target: string;
  index: number;
  width: number;
  pxPerBeat: number;
  viewport?: TimelineViewport;
  compact?: boolean;
  playlistTop?: number;
}) {
  const [visible, setVisible] = createSignal(!props.viewport?.supported);
  const view = createMemo(() => gainAutomationView(props.curve));
  const domain = createMemo(() => gainAutomationDomain(props.curve));
  const clock = createMemo(() => projectClock(store.project.properties));
  const at = (p: GainAutomation["time_range"]["start"]) =>
    clock().quarterAt(p, props.curve.ppq ?? AUTOMATION_CURVES.gain.defaultPpq);
  const start = () => at(props.curve.time_range.start),
    end = () => at(props.curve.time_range.end);
  const origin = () =>
    props.compact
      ? props.curve.points.reduce(
          (lowest, point) => Math.min(lowest, at(point.position)),
          start(),
        )
      : 0;
  const plotted = (p: GainAutomation["time_range"]["start"]) =>
    at(p) - origin();
  const activeId = createUniqueId();
  const inPlaylist = () => props.playlistTop !== undefined;
  const polyline = createMemo(() =>
    lanePolyline(
      view(),
      plotted,
      props.pxPerBeat,
      domain(),
      AUTOMATION_ROW_HEIGHT,
    ),
  );
  return (
    <div
      class="gain-automation-preview"
      classList={{ "playlist-gain-lane": inPlaylist() }}
      ref={(node) => props.viewport?.observe(node, setVisible)}
      style={{
        height: `${AUTOMATION_ROW_HEIGHT}px`,
        width: `${props.width}px`,
        top: inPlaylist() ? `${props.playlistTop}px` : undefined,
      }}
      data-gain-owner={props.target}
      data-gain-index={props.index}
    >
      <Show when={visible()}>
        <svg
          width={props.width}
          height={AUTOMATION_ROW_HEIGHT}
          aria-hidden="true"
        >
          <defs>
            <clipPath id={activeId}>
              <rect
                x={(start() - origin()) * props.pxPerBeat}
                y="0"
                width={(end() - start()) * props.pxPerBeat}
                height={AUTOMATION_ROW_HEIGHT}
              />
            </clipPath>
          </defs>
          <Show when={!inPlaylist()}>
            <polyline
              class="automation-polyline gain-envelope-context-curve"
              points={polyline()}
            />
          </Show>
          <polyline
            class="automation-polyline"
            clip-path={`url(#${activeId})`}
            points={polyline()}
          />
        </svg>
        <For each={view().points}>
          {(point, index) => {
            const xy = () =>
              pointXY(
                point,
                plotted,
                props.pxPerBeat,
                domain(),
                AUTOMATION_ROW_HEIGHT,
                view().value_mapping,
              );
            const raw = () => pointValue(point.value) ?? 0,
              applied = () =>
                mappedAutomationValue(raw(), view().value_mapping);
            return (
              <Show when={!inPlaylist() || (at(point.position) >= start() && at(point.position) <= end())}>
              <button
                type="button"
                class="automation-point gain-envelope-anchor"
                classList={{
                  "gain-envelope-context":
                    at(point.position) < start() || at(point.position) >= end(),
                }}
                style={{ left: `${xy().x}px`, top: `${xy().y}px` }}
                aria-label={i18n.t("gainEditor.anchorValue", {
                  index: index() + 1,
                  raw: raw(),
                  applied: applied(),
                })}
                title={i18n.t("gainEditor.anchorValue", {
                  index: index() + 1,
                  raw: raw(),
                  applied: applied(),
                })}
                onClick={(event) => {
                  event.stopPropagation();
                  gainAutomationUi.open(props.target, props.index, index());
                }}
              />
              </Show>
            );
          }}
        </For>
        <button
          type="button"
          class="gain-envelope-edit"
          style={inPlaylist() ? {
            left: `${start() * props.pxPerBeat}px`,
            "max-width": `${(end() - start()) * props.pxPerBeat}px`,
          } : undefined}
          onClick={() => gainAutomationUi.open(props.target, props.index)}
        >
          {i18n.t("gainEditor.edit")}
        </button>
      </Show>
    </div>
  );
}
