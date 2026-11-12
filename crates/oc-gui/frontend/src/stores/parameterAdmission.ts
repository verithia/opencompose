import type { ParameterTargetStamp } from "../types/project";
import { i18n } from "./i18n";

/** One interaction's document and opaque backend target admission. */
export type ParameterRequestOwner = (() => boolean) & {
  readonly expectedStamp: ParameterTargetStamp | null;
  requireStamp(): ParameterTargetStamp;
};

export function captureParameterAdmission(
  expectedStamp: ParameterTargetStamp | null,
  isCurrent: () => boolean,
): ParameterRequestOwner {
  const stamp = typeof expectedStamp === "string" && expectedStamp.length > 0 ? expectedStamp : null;
  const owns = () => stamp !== null && isCurrent();
  return Object.assign(owns, {
    expectedStamp: stamp,
    requireStamp: () => {
      if (!owns() || stamp === null) throw new Error(i18n.t("synth.ui.ownerChanged"));
      return stamp;
    },
  });
}

export function withParameterTarget(owner: ParameterRequestOwner, isCurrent: () => boolean): ParameterRequestOwner {
  return captureParameterAdmission(owner.expectedStamp, () => owner() && isCurrent());
}

export function isParameterRequestOwner(owner: () => boolean): owner is ParameterRequestOwner {
  return "expectedStamp" in owner && "requireStamp" in owner;
}
