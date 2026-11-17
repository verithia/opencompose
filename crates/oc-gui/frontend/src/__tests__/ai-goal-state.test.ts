import { describe, expect, it } from "vitest";
import { createGoalStore, EMPTY_GOAL_PLAN, reduceState, reduceStates } from "../ai/goalState";
import type { SessionState } from "../ai/messageProtocol";

const goalSet = (text: string): SessionState => ({ kind: "goal_set", text, author: "person" });
const plan = (items: { text: string; status: "pending" | "in_progress" | "done" }[]): SessionState =>
  ({ kind: "plan_set", items });

describe("goal and plan reduction", () => {
  it("changes only the kind a state row names", () => {
    const withPlan = reduceState({ ...EMPTY_GOAL_PLAN }, plan([{ text: "write", status: "pending" }]));
    expect(withPlan).toEqual({ goal: null, plan: [{ text: "write", status: "pending" }] });
    expect(reduceState(withPlan, goalSet("finish the bridge"))).toEqual({
      goal: "finish the bridge",
      plan: [{ text: "write", status: "pending" }],
    });
    expect(reduceState(withPlan, { kind: "goal_cleared", author: "agent" })).toEqual({
      goal: null,
      plan: [{ text: "write", status: "pending" }],
    });
  });

  it("reads the newest rows in order and keeps an empty plan empty", () => {
    expect(reduceStates([
      goalSet("first"),
      plan([]),
      goalSet("second"),
      plan([{ text: "mix", status: "in_progress" }]),
    ])).toEqual({ goal: "second", plan: [{ text: "mix", status: "in_progress" }] });
  });

  it("applies live rows and installs a whole view without mixing owners", () => {
    const store = createGoalStore();
    store.apply(goalSet("first"));
    store.apply(plan([{ text: "write", status: "done" }]));
    expect(store.state()).toEqual({ goal: "first", plan: [{ text: "write", status: "done" }] });
    store.load([{ kind: "goal_cleared", author: "person" }]);
    expect(store.state()).toEqual({ goal: null, plan: null });
    store.apply(goalSet("again"));
    store.reset();
    expect(store.state()).toEqual(EMPTY_GOAL_PLAN);
  });
});
