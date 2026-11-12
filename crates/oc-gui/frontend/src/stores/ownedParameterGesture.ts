import type { ParameterTargetStamp } from "../types/project";
import type { AutomationWriteSurface } from "./automationWrite";
import type { OwnedParameterGestureEnd, UndoGestureToken } from "./api";
import type { AutomationPosition } from "./synthAutomation";
import { i18n } from "./i18n";

interface OwnedParameterSurfaceOptions {
  key: string;
  label: string;
  value(): number;
  position(quarter: number): AutomationPosition;
  isCurrent(): boolean;
  expectedStamp: ParameterTargetStamp | null;
  begin(expectedStamp: ParameterTargetStamp): Promise<UndoGestureToken>;
  point(token: UndoGestureToken, position: AutomationPosition, value: number): Promise<unknown>;
  end(token: UndoGestureToken): Promise<OwnedParameterGestureEnd>;
  apply(result: OwnedParameterGestureEnd): void;
}

/** One backend session belongs to the source captured when the interaction starts. */
export function ownedParameterSurface(options: OwnedParameterSurfaceOptions): AutomationWriteSurface {
  let token: UndoGestureToken | null = null;
  const requireCurrent = () => {
    if (!options.isCurrent()) throw new Error(i18n.t("synth.ui.ownerChanged"));
  };
  return {
    key: options.key,
    label: options.label,
    value: options.value,
    position: options.position,
    open: async () => {
      requireCurrent();
      if (typeof options.expectedStamp !== "string" || options.expectedStamp.length === 0) throw new Error(i18n.t("synth.ui.ownerChanged"));
      token = await options.begin(options.expectedStamp);
      // Retain even a stale reply's token so close releases precisely this claim.
      requireCurrent();
    },
    point: async (position, value) => {
      requireCurrent();
      if (!token) throw new Error(i18n.t("synth.ui.ownerChanged"));
      await options.point(token, position, value);
    },
    close: async () => {
      const closing = token;
      token = null;
      if (!closing) return;
      const result = await options.end(closing);
      if (result.closed && options.isCurrent()) options.apply(result);
    },
  };
}
