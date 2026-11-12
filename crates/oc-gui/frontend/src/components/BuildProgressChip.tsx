import { Show } from "solid-js";
import type { BuildSnapshot } from "../types/transport";
import { i18n } from "../stores/i18n";

/**
 * What a project rebuild is doing: the stage, the instrument being prepared and
 * how far it has got. It replaces a bare spinner, so a slow plugin load names
 * itself instead of looking like a hang.
 */
export function BuildProgressChip(props: { build: BuildSnapshot | null | undefined }) {
  const text = (build: BuildSnapshot): string => {
    const stage = i18n.t(`build.stage.${build.stage}` as Parameters<typeof i18n.t>[0]);
    if (build.stage === "soundfont") {
      // A bank preparation reports its own stage; the count is the samples it
      // has decoded so far.
      const detail = i18n.t(`build.soundfont.${build.label}` as Parameters<typeof i18n.t>[0]);
      const text = `${stage} · ${detail}`;
      return build.total > 0
        ? i18n.t("build.stageWithCount", { stage: text, done: build.done, total: build.total })
        : text;
    }
    if (build.stage === "instruments" && build.label) {
      return i18n.t("build.instrument", {
        stage,
        name: build.label,
        done: build.done + 1,
        total: build.total,
      });
    }
    return build.total > 0
      ? i18n.t("build.stageWithCount", { stage, done: build.done, total: build.total })
      : stage;
  };

  return (
    <Show when={props.build && props.build.stage !== "idle" ? props.build : null}>
      {(build) => (
        <span class="status-chip loading build-progress" role="status" aria-live="polite">
          <span class="build-progress-sweep" aria-hidden="true" />
          <span class="build-progress-text">{text(build())}</span>
        </span>
      )}
    </Show>
  );
}
