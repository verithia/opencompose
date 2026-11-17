import { describe, expect, it } from "vitest";
import { createSignal } from "solid-js";
import { createBindingFollower } from "../ai/bindingFollower";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

function harness() {
  const [openId, setOpenId] = createSignal<string | null>("s1");
  let epoch = 1;
  const statuses: ReturnType<typeof deferred<unknown>>[] = [];
  const warnings: string[] = [];
  const follower = createBindingFollower({
    status: () => {
      const next = deferred<unknown>();
      statuses.push(next);
      return next.promise;
    },
    bind: async () => undefined,
    openId,
    epoch: () => epoch,
    disposed: () => false,
    onWarn: (message) => warnings.push(message),
  });
  return {
    follower,
    setOpenId,
    statuses,
    warnings,
    bumpEpoch: () => { epoch += 1; },
  };
}

describe("the binding follower", () => {
  it("keeps the newest answer when a late read arrives after a rebind", async () => {
    const { follower, statuses } = harness();
    const read = follower.read("s1");
    const follow = follower.follow("s1");
    statuses[1].resolve({ bound: "/songs/old.ocp", open: "/songs/new.ocp", matches: false });
    await follow;
    expect(follower.binding()).toEqual({ bound: "/songs/new.ocp", open: "/songs/new.ocp", matches: true });

    statuses[0].resolve({ bound: "/songs/old.ocp", open: "/songs/old.ocp", matches: true });
    await read;
    expect(follower.binding()).toEqual({ bound: "/songs/new.ocp", open: "/songs/new.ocp", matches: true });
  });

  it("drops an answer from an epoch the panel has left", async () => {
    const { follower, statuses, bumpEpoch } = harness();
    const read = follower.read("s1");
    bumpEpoch();
    statuses[0].resolve({ bound: "/songs/a.ocp", open: "/songs/b.ocp", matches: false });
    await read;
    expect(follower.binding()).toBeNull();
  });

  it("forgets the binding on a reset even while a read is in flight", async () => {
    const { follower, statuses } = harness();
    const read = follower.read("s1");
    follower.reset();
    statuses[0].resolve({ bound: "/songs/a.ocp", open: "/songs/b.ocp", matches: false });
    await read;
    expect(follower.binding()).toBeNull();
  });
});
