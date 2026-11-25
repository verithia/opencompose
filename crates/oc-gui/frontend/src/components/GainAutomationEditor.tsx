/** Raw authored anchors remain editable when an output mapping has no inverse. */
import {
  For,
  Index,
  Show,
  createEffect,
  createMemo,
  createSignal,
  on,
  onCleanup,
} from "solid-js";
import type {
  AutomationCurve,
  BarBeat,
  GainAutomation,
  AutomationPoint,
  AutomationValueMapping,
} from "../types/project";
import { store } from "../stores/project";
import { i18n } from "../stores/i18n";
import { gainAutomationUi } from "../stores/gainAutomationUi";
import { AUTOMATION_CURVES } from "../data/automationCurves.gen";
import { gainAutomationGroups } from "../utils/gainAutomationView";
import { mappedAutomationValue, pointValue } from "../utils/automationRow";
import { projectClock } from "../utils/projectClock";
import { GainAutomationPreview } from "./GainAutomationPreview";

const clone = <T,>(value: T): T => JSON.parse(JSON.stringify(value));
const SHAPES: AutomationCurve[] = [
  "linear",
  "stairs",
  "half_sine",
  "smooth",
  "exponential",
];
export function GainAutomationEditor(props: { target: string }) {
  const curves = createMemo(
    () =>
      gainAutomationGroups(store.project).find(
        (group) => group.target === props.target,
      )?.curves ?? [],
  );
  return (
    <Index each={curves()}>
      {(curve, index) => (
        <GainCurveEditor target={props.target} index={index} curve={curve()} />
      )}
    </Index>
  );
}

function GainCurveEditor(props: {
  target: string;
  index: number;
  curve: GainAutomation;
}) {
  let section: HTMLElement | undefined;
  let submission = 0;
  onCleanup(() => {
    submission++;
  });
  const currentAdmission = () => ({
    stamp: store.parameterTargetStamp(),
    revision: store.projectRevision,
    target: props.target,
    index: props.index,
  });
  const [admission, setAdmission] = createSignal(currentAdmission());
  const [base, setBase] = createSignal(clone(props.curve)),
    [draft, setDraft] = createSignal(clone(props.curve));
  const [open, setOpen] = createSignal(false),
    [busy, setBusy] = createSignal(false),
    [error, setError] = createSignal<string | null>(null);
  createEffect(
    on(
      () =>
        [
          props.target,
          props.index,
          store.parameterTargetStamp(),
          store.documentIdentity().generation,
          store.loading(),
        ] as const,
      () => {
        submission++;
        setBusy(false);
      },
    ),
  );
  const dirty = () => JSON.stringify(draft()) !== JSON.stringify(base());
  createEffect(
    on(
      () =>
        [
          props.target,
          props.index,
          store.parameterTargetStamp(),
          JSON.stringify(props.curve),
        ] as const,
      ([, , , serialized]) => {
        const next = JSON.parse(serialized) as GainAutomation;
        if (!dirty()) {
          setBase(next);
          setDraft(clone(next));
          setAdmission(currentAdmission());
        }
      },
    ),
  );
  createEffect(() => {
    const request = gainAutomationUi.request();
    if (request?.target === props.target && request.index === props.index) {
      setOpen(true);
      queueMicrotask(() => {
        if (section?.isConnected && request.pointIndex !== undefined)
          section
            .querySelector<HTMLInputElement>(
              `[data-gain-anchor-index="${request.pointIndex}"] [data-gain-raw-anchor]`,
            )
            ?.focus();
        if (gainAutomationUi.request() === request) gainAutomationUi.clear();
      });
    }
  });
  const segments = () => draft().interpolation === "Segments";
  const update = (change: Partial<GainAutomation>) =>
    setDraft((current) => ({ ...current, ...change }));
  const point = (index: number, change: Partial<AutomationPoint>) =>
    update({
      points: draft().points.map((p, i) =>
        i === index ? { ...p, ...change } : p,
      ),
    });
  const mapping = (change: Partial<AutomationValueMapping>) =>
    update({
      value_mapping: { curve: "linear", ...draft().value_mapping, ...change },
    });
  const at = (position: BarBeat) =>
    projectClock(store.project.properties).quarterAt(
      position,
      draft().ppq ?? AUTOMATION_CURVES.gain.defaultPpq,
    );
  const extent = () => {
    try {
      let low = at(draft().time_range.start),
        high = at(draft().time_range.end);
      for (const p of draft().points) {
        const q = at(p.position);
        low = Math.min(low, q);
        high = Math.max(high, q);
      }
      return high - low || 1;
    } catch {
      return 1;
    }
  };
  const validPreview = () => {
    try {
      at(draft().time_range.start);
      at(draft().time_range.end);
      draft().points.forEach((p) => at(p.position));
      return true;
    } catch {
      return false;
    }
  };
  const reset = () => {
    submission++;
    setBusy(false);
    const next = clone(props.curve);
    setBase(next);
    setDraft(clone(next));
    setAdmission(currentAdmission());
    setError(null);
  };
  const number = (value: string, apply: (value: number) => void) => {
    const next = Number(value);
    if (value.trim() !== "" && Number.isFinite(next)) apply(next);
  };
  const positionFields = (
    value: BarBeat,
    change: (value: BarBeat) => void,
    label: string,
  ) => (
    <span class="gain-envelope-position">
      <For
        each={
          [
            ["bar", "gainEditor.bar"],
            ["beat", "gainEditor.beat"],
            ["subdivision", "gainEditor.tick"],
          ] as const
        }
      >
        {([key, copy]) => (
          <label>
            {i18n.t(copy)}
            <input
              type="number"
              step="1"
              min={key === "subdivision" ? 0 : 1}
              value={value[key]}
              aria-label={`${label} ${i18n.t(copy)}`}
              onChange={(e) =>
                number(e.currentTarget.value, (n) =>
                  change({ ...value, [key]: n }),
                )
              }
            />
          </label>
        )}
      </For>
    </span>
  );
  const apply = async () => {
    if (busy()) return;
    const captured = admission(),
      submitted = draft(),
      sequence = ++submission;
    const documentOwner = store.parameterRequestOwner();
    const owns = () =>
      documentOwner() &&
      sequence === submission &&
      props.target === captured.target &&
      props.index === captured.index &&
      store.parameterTargetStamp() === captured.stamp;
    setBusy(true);
    setError(null);
    try {
      if (
        !captured.stamp ||
        captured.stamp !== store.parameterTargetStamp() ||
        captured.revision !== store.projectRevision ||
        captured.target !== props.target ||
        captured.index !== props.index
      )
        throw { code: "stale" };
      const applied = await store.replaceGainAutomation(
        captured.target,
        captured.index,
        base(),
        submitted,
        captured.stamp,
        captured.revision,
      );
      if (
        owns() &&
        draft() === submitted &&
        store.projectRevision === applied.revision
      ) {
        const authoritative = clone(props.curve);
        setBase(authoritative);
        setDraft(clone(authoritative));
        setAdmission(currentAdmission());
      }
    } catch (cause) {
      if (owns() && draft() === submitted) {
        const failure = cause as { code?: string; message?: string };
        setError(
          failure?.code === "stale"
            ? i18n.t("gainEditor.stale")
            : (failure?.message ?? String(cause)),
        );
      }
    } finally {
      if (owns()) setBusy(false);
    }
  };
  return (
    <section
      ref={(node) => {
        section = node;
      }}
      class="gain-envelope-editor"
      data-gain-editor-owner={props.target}
      data-gain-editor-index={props.index}
    >
      <button
        type="button"
        class="gain-envelope-summary"
        aria-expanded={open()}
        onClick={() => setOpen(!open())}
      >
        {i18n.t("gainEditor.title", { index: props.index + 1 })} ·{" "}
        {i18n.t(
          draft().interpolation === "Step"
            ? "device.automation.curve.stairs"
            : draft().interpolation === "Linear"
              ? "device.automation.curve.linear"
              : "gainEditor.segments",
        )}
      </button>
      <Show when={open()}>
        <p>{i18n.t("gainEditor.rawHint")}</p>
        <Show
          when={draft().value_mapping?.input_quantization_steps !== undefined}
        >
          <p>{i18n.t("gainEditor.quantizedHint")}</p>
        </Show>
        <Show when={validPreview()}>
          <GainAutomationPreview
            curve={draft()}
            target={props.target}
            index={props.index}
            width={280}
            pxPerBeat={280 / extent()}
            compact
          />
        </Show>
        <label>
          {i18n.t("gainEditor.mode")}
          <select
            value={draft().interpolation}
            onChange={(e) => {
              const mode = e.currentTarget
                .value as GainAutomation["interpolation"];
              if (mode === "Segments")
                update({
                  interpolation: mode,
                  points: draft().points.map((p) => ({
                    ...p,
                    curve:
                      draft().interpolation === "Step" ? "stairs" : p.curve,
                  })),
                });
              else
                update({
                  interpolation: mode,
                  value_mapping: undefined,
                  preserve_discontinuities: false,
                  points: draft().points.map(
                    ({
                      curve: _curve,
                      tension: _tension,
                      curvature: _curvature,
                      ...p
                    }) => p,
                  ),
                });
            }}
          >
            <option value="Step">
              {i18n.t("device.automation.curve.stairs")}
            </option>
            <option value="Linear">
              {i18n.t("device.automation.curve.linear")}
            </option>
            <option value="Segments">{i18n.t("gainEditor.segments")}</option>
          </select>
        </label>
        <label>
          {i18n.t("gainEditor.ppq")}
          <input
            type="number"
            step="1"
            value={draft().ppq ?? AUTOMATION_CURVES.gain.defaultPpq}
            onChange={(e) =>
              number(e.currentTarget.value, (n) => update({ ppq: n }))
            }
          />
        </label>
        <div>
          {i18n.t("gainEditor.start")}
          {positionFields(
            draft().time_range.start,
            (start) => update({ time_range: { ...draft().time_range, start } }),
            i18n.t("gainEditor.start"),
          )}
        </div>
        <div>
          {i18n.t("gainEditor.end")}
          {positionFields(
            draft().time_range.end,
            (end) => update({ time_range: { ...draft().time_range, end } }),
            i18n.t("gainEditor.end"),
          )}
        </div>
        <Show when={segments()}>
          <label>
            <input
              type="checkbox"
              checked={draft().preserve_discontinuities ?? false}
              onChange={(e) =>
                update({ preserve_discontinuities: e.currentTarget.checked })
              }
            />
            {i18n.t("gainEditor.jumps")}
          </label>
          <label>
            {i18n.t("device.automation.mapping")}
            <select
              value={draft().value_mapping?.curve ?? "none"}
              onChange={(e) => {
                const kind = e.currentTarget.value as
                  | "none"
                  | "linear"
                  | "exponential";
                if (kind === "none") {
                  update({ value_mapping: undefined });
                  return;
                }
                const { curvature, ...previous } = draft().value_mapping ?? {
                  curve: "linear",
                };
                update({
                  value_mapping: {
                    ...previous,
                    curve: kind,
                    ...(kind === "exponential" && curvature !== undefined
                      ? { curvature }
                      : {}),
                  },
                });
              }}
            >
              <option value="none">{i18n.t("gainEditor.none")}</option>
              <option value="linear">
                {i18n.t("device.automation.curve.linear")}
              </option>
              <option value="exponential">
                {i18n.t("device.automation.curve.exponential")}
              </option>
            </select>
          </label>
          <Show when={draft().value_mapping}>
            <For
              each={
                [
                  ["scale", "gainEditor.scale", 1],
                  ["offset", "gainEditor.offset", 0],
                  ["curvature", "device.automation.curvature", 0],
                ] as const
              }
            >
              {([key, copy, fallback]) => (
                <Show
                  when={
                    key !== "curvature" ||
                    draft().value_mapping?.curve === "exponential"
                  }
                >
                  <label>
                    {i18n.t(copy)}
                    <input
                      type="number"
                      step="any"
                      value={draft().value_mapping?.[key] ?? fallback}
                      onChange={(e) =>
                        number(e.currentTarget.value, (n) =>
                          mapping({ [key]: n }),
                        )
                      }
                    />
                  </label>
                </Show>
              )}
            </For>
            <label>
              {i18n.t("gainEditor.steps")}
              <input
                type="number"
                step="1"
                min={AUTOMATION_CURVES.inputQuantizationSteps.min}
                max={AUTOMATION_CURVES.inputQuantizationSteps.max}
                value={draft().value_mapping?.input_quantization_steps ?? ""}
                onChange={(e) => {
                  const raw = e.currentTarget.value;
                  if (raw === "")
                    mapping({ input_quantization_steps: undefined });
                  else
                    number(raw, (n) =>
                      mapping({ input_quantization_steps: n }),
                    );
                }}
              />
            </label>
          </Show>
        </Show>
        <For each={draft().points}>
          {(anchor, index) => (
            <div class="gain-envelope-point" data-gain-anchor-index={index()}>
              <strong>{index() + 1}</strong>
              {positionFields(
                anchor.position,
                (position) => point(index(), { position }),
                i18n.t("gainEditor.anchor", { index: index() + 1 }),
              )}
              <label>
                {i18n.t("gainEditor.raw")}
                <input
                  data-gain-raw-anchor
                  type="number"
                  step="any"
                  value={pointValue(anchor.value) ?? 0}
                  aria-label={i18n.t("gainEditor.rawAnchor", {
                    index: index() + 1,
                  })}
                  onChange={(e) =>
                    number(e.currentTarget.value, (n) =>
                      point(index(), { value: { Number: n } }),
                    )
                  }
                />
              </label>
              <output>
                {i18n.t("gainEditor.applied")}{" "}
                {mappedAutomationValue(
                  pointValue(anchor.value) ?? 0,
                  segments() ? draft().value_mapping : undefined,
                ).toPrecision(5)}
              </output>
              <Show when={segments()}>
                <select
                  value={anchor.curve ?? "linear"}
                  aria-label={i18n.t("gainEditor.anchorCurve", {
                    index: index() + 1,
                  })}
                  onChange={(e) => {
                    const curve = e.currentTarget.value as AutomationCurve;
                    point(index(), {
                      curve,
                      tension:
                        curve === "smooth" ? (anchor.tension ?? 1) : undefined,
                      curvature:
                        curve === "exponential"
                          ? (anchor.curvature ?? 0)
                          : undefined,
                    });
                  }}
                >
                  <For each={SHAPES}>
                    {(shape) => (
                      <option value={shape}>
                        {i18n.t(`device.automation.curve.${shape}`)}
                      </option>
                    )}
                  </For>
                </select>
                <Show when={anchor.curve === "exponential"}>
                  <input
                    type="number"
                    step="any"
                    aria-label={i18n.t("gainEditor.anchorCurvature", {
                      index: index() + 1,
                    })}
                    min={AUTOMATION_CURVES.curvature.min}
                    max={AUTOMATION_CURVES.curvature.max}
                    value={
                      anchor.curvature ?? AUTOMATION_CURVES.curvature.default
                    }
                    onChange={(e) =>
                      number(e.currentTarget.value, (n) =>
                        point(index(), { curvature: n }),
                      )
                    }
                  />
                </Show>
                <Show when={anchor.curve === "smooth"}>
                  <input
                    type="number"
                    step="any"
                    min="0"
                    max="1"
                    aria-label={i18n.t("gainEditor.anchorTension", {
                      index: index() + 1,
                    })}
                    value={anchor.tension ?? 1}
                    onChange={(e) =>
                      number(e.currentTarget.value, (n) =>
                        point(index(), { tension: n }),
                      )
                    }
                  />
                </Show>
              </Show>
              <button
                type="button"
                onClick={() =>
                  update({
                    points: draft().points.filter((_, i) => i !== index()),
                  })
                }
              >
                {i18n.t("gainEditor.remove")}
              </button>
            </div>
          )}
        </For>
        <button
          type="button"
          onClick={() =>
            update({
              points: [
                ...draft().points,
                clone(
                  draft().points.at(-1) ?? {
                    position: draft().time_range.start,
                    value: { Number: 1 },
                  },
                ),
              ],
            })
          }
        >
          {i18n.t("gainEditor.add")}
        </button>
        <Show when={error()}>
          <p role="alert">{error()}</p>
        </Show>
        <button
          type="button"
          disabled={busy() || !dirty()}
          onClick={() => void apply()}
        >
          {i18n.t("gainEditor.apply")}
        </button>
        <button type="button" onClick={reset}>
          {i18n.t("gainEditor.reset")}
        </button>
      </Show>
    </section>
  );
}
