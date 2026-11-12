/**
 * The session's permission for a call that changes the project DSL: a three-way
 * segmented control. The panel shows it in the session bar and the settings
 * dialog shows the same control beside the selected mode's description.
 */
import { For } from "solid-js";
import { i18n, type I18nKey } from "../stores/i18n";
import { PERMISSION_MODES, type PermissionMode } from "../ai/permission";
import { useTooltip } from "./Tooltip";

const LABEL: Readonly<Record<PermissionMode, I18nKey>> = {
  require_approval: "ai.permission.require_approval",
  autoreview: "ai.permission.autoreview",
  write: "ai.permission.write",
};

const HINT: Readonly<Record<PermissionMode, I18nKey>> = {
  require_approval: "ai.permission.require_approval.hint",
  autoreview: "ai.permission.autoreview.hint",
  write: "ai.permission.write.hint",
};

/** The one-line description of a mode, for a surface with room to show it. */
export function permissionHint(mode: PermissionMode): I18nKey {
  return HINT[mode];
}

export function AiPermissionSelect(props: {
  value: PermissionMode;
  onChange: (mode: PermissionMode) => void;
  disabled?: boolean;
}) {
  return (
    <div class="ai-permission" role="group" aria-label={i18n.t("ai.permission.label")}>
      <For each={PERMISSION_MODES}>
        {(mode) => (
          <button
            type="button"
            class="ai-permission-option"
            classList={{ active: props.value === mode }}
            aria-pressed={props.value === mode}
            data-permission={mode}
            disabled={props.disabled}
            onClick={() => props.onChange(mode)}
            {...useTooltip(() => i18n.t(HINT[mode]))}
          >{i18n.t(LABEL[mode])}</button>
        )}
      </For>
    </div>
  );
}
