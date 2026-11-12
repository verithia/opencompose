import { store } from "../stores/project";
import { i18n } from "../stores/i18n";
import type { Effect } from "../types/project";
import { openContextMenu } from "./ContextMenu";

export interface EffectMenuOptions {
  x: number;
  y: number;
  target: string;
  index: number;
  /** Effects in the chain, so the moves know where the ends are. */
  count: number;
  effect: Effect;
  /** Applies a move; the rack owns it so the open panel follows the effect. */
  onMove?: (from: number, to: number) => void;
}

export function openEffectContextMenu(options: EffectMenuOptions) {
  const bypassed = options.effect.bypass === true;
  const canMoveUp = options.index > 0;
  const canMoveDown = options.index < options.count - 1;
  const move = (to: number) => {
    if (options.onMove) options.onMove(options.index, to);
    else void store.moveEffect(options.target, options.index, to);
  };
  openContextMenu({
    x: options.x,
    y: options.y,
    className: "effect-context-menu",
    ariaLabel: i18n.t("menu.actions"),
    items: [
      {
        label: i18n.t("menu.effect.bypass"),
        checked: bypassed,
        checkType: "checkbox",
        onSelect: () => {
          void store.setEffectBypass(options.target, options.index, !bypassed);
        },
      },
      { type: "separator" },
      {
        label: i18n.t("menu.effect.moveUp"),
        disabled: !canMoveUp,
        hint: canMoveUp ? undefined : i18n.t("menu.effect.moveUpHint"),
        onSelect: () => move(options.index - 1),
      },
      {
        label: i18n.t("menu.effect.moveDown"),
        disabled: !canMoveDown,
        hint: canMoveDown ? undefined : i18n.t("menu.effect.moveDownHint"),
        onSelect: () => move(options.index + 1),
      },
      { type: "separator" },
      {
        label: i18n.t("menu.effect.remove"),
        danger: true,
        onSelect: () => void store.removeEffect(options.target, options.index),
      },
    ],
  });
}
