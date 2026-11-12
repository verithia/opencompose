import type { ProjectSnapshot } from "./project";

/** A stale close never consumes a newer automation session. */
export interface OwnedParameterGestureEnd {
  closed: boolean;
  snapshot: ProjectSnapshot;
}

/** A live-only touch claim, distinct from history and parameter admission. */
export type EffectTouchToken = string & { readonly __effectTouchToken: unique symbol };
