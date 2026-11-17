/** The conversation's goal and plan, derived from its typed session rows. */
import { createSignal } from "solid-js";
import type { PlanItem, SessionState } from "./messageProtocol";

export interface GoalPlan {
  goal: string | null;
  plan: PlanItem[] | null;
}

export const EMPTY_GOAL_PLAN: GoalPlan = { goal: null, plan: null };

/** One typed change applied to the goal and plan it names; the other stays. */
export function reduceState(current: GoalPlan, state: SessionState): GoalPlan {
  switch (state.kind) {
    case "goal_set": return { ...current, goal: state.text };
    case "goal_cleared": return { ...current, goal: null };
    case "plan_set": return { ...current, plan: state.items.length ? state.items : null };
  }
}

/** The newest goal and plan the given rows hold. */
export function reduceStates(states: readonly SessionState[]): GoalPlan {
  return states.reduce(reduceState, EMPTY_GOAL_PLAN);
}

/** One owner for the goal and plan the composer shows. */
export function createGoalStore() {
  const [state, setState] = createSignal<GoalPlan>(EMPTY_GOAL_PLAN);
  return {
    state,
    reset(): void {
      setState(EMPTY_GOAL_PLAN);
    },
    /** Apply one typed change, in the order the conversation saved it. */
    apply(change: SessionState): void {
      setState(previous => reduceState(previous, change));
    },
    /** Install the whole view's latest state, as `sessions_open` reports it. */
    load(states: readonly SessionState[]): void {
      setState(reduceStates(states));
    },
  };
}
