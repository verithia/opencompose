/**
 * LatencyReadout — the three-part latency readout for the selected track.
 * (a) the track's reported latency with the manual offset kept separate,
 * (b) the reference each destination is compared against,
 * (c) the compensation the plan applies at that destination.
 */
import { For, Show } from "solid-js";
import type { TrackLatencyReadoutResponse } from "../stores/api";
import { i18n } from "../stores/i18n";

function signed(samples: number): string {
  return samples > 0 ? `+${samples}` : `${samples}`;
}

export function LatencyReadout(props: { readout: TrackLatencyReadoutResponse | null }) {
  const readout = () => props.readout?.readout;
  return (
    <Show when={readout()}>
      {(value) => (
        <div
          class="latency-readout"
          data-track={value().track}
          style={{ "font-size": "10px", "min-width": "180px" }}
        >
          <div style={{ "font-weight": "500", "margin-bottom": "4px", color: "var(--text-muted)" }}>
            {i18n.t("latency.title")}
          </div>
          <div style={{ display: "flex", gap: "8px", "flex-wrap": "wrap" }}>
            <span>
              {i18n.t("latency.plugins")}{" "}
              <span data-latency="reported" style={{ "font-family": "var(--font-mono)" }}>
                {value().reported_samples}
              </span>{" "}
              smp
            </span>
            <span>
              {i18n.t("latency.manual")}{" "}
              <span data-latency="manual" style={{ "font-family": "var(--font-mono)" }}>
                {signed(value().applied_offset_samples)}
              </span>{" "}
              smp
            </span>
          </div>
          <For each={value().destinations}>
            {(destination) => (
              <div
                class="latency-readout-destination"
                data-destination={destination.destination}
                style={{ display: "flex", gap: "8px", "flex-wrap": "wrap", "margin-top": "2px" }}
              >
                <span style={{ color: "var(--text-muted)" }}>→ {destination.destination}</span>
                <span>
                  {i18n.t("latency.reference")}{" "}
                  <span data-latency="reference" style={{ "font-family": "var(--font-mono)" }}>
                    {destination.reference_samples}
                  </span>{" "}
                  smp
                </span>
                <span>
                  {i18n.t("latency.compensation")}{" "}
                  <span data-latency="compensation" style={{ "font-family": "var(--font-mono)" }}>
                    {destination.compensation_samples}
                  </span>{" "}
                  smp
                </span>
                <Show when={destination.diagnostic_delta !== null}>
                  <span data-latency="diagnostic" style={{ color: "var(--text-secondary)" }}>
                    {i18n.t("latency.makeupDelay", { delta: destination.diagnostic_delta ?? 0 })}
                  </span>
                </Show>
              </div>
            )}
          </For>
          <Show when={value().offset_clamped}>
            <div data-latency="clamped" style={{ "margin-top": "2px", color: "var(--danger, #d08b2c)" }}>
              {i18n.t("latency.clamped")}
            </div>
          </Show>
          <Show when={value().bypassed}>
            <div data-latency="bypassed" style={{ "margin-top": "2px", color: "var(--text-muted)" }}>
              {i18n.t("latency.bypassed")}
            </div>
          </Show>
          <Show when={props.readout?.note}>
            <div data-latency="note" style={{ "margin-top": "2px", color: "var(--text-muted)" }}>
              {props.readout?.note}
            </div>
          </Show>
        </div>
      )}
    </Show>
  );
}
