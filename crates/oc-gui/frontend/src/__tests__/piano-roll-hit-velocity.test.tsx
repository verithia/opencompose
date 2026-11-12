/**
 * Drum hits in the velocity lane are writable: a drag writes one exact
 * velocity and Alt drops it back to the written dynamic.
 */
import { fireEvent, render } from "@solidjs/testing-library";
import { describe, expect, it, vi } from "vitest";
import { VelocityLane, type DisplayHitBlock } from "../components/PianoRoll";

const HITS: DisplayHitBlock[] = [
  {
    beat: 0,
    velocity: 88,
    velocitySource: "velocity",
    dynamics: null,
    label: "kick",
    hitIndex: 0,
    outOfRange: false,
  },
  {
    beat: 2,
    velocity: 64,
    velocitySource: "dynamics",
    dynamics: "MP",
    label: "snare",
    hitIndex: 1,
    outOfRange: false,
  },
];

function mount(handlers: {
  onSetHitVelocity?: (hit: DisplayHitBlock, velocity: number) => Promise<string | null>;
  onClearHitVelocity?: (hit: DisplayHitBlock) => Promise<string | null>;
}) {
  return render(() => (
    <VelocityLane
      blocks={[]}
      hits={HITS}
      selected={[]}
      pxPerBeat={20}
      xForBeat={(beat) => beat * 20}
      width={400}
      toolFailure={null}
      onSelect={() => {}}
      onSetVelocity={async () => null}
      onSetVelocities={async () => null}
      onClearVelocity={async () => null}
      onSetHitVelocity={handlers.onSetHitVelocity ?? (async () => null)}
      onClearHitVelocity={handlers.onClearHitVelocity ?? (async () => null)}
    />
  ));
}

describe("velocity lane hit bars", () => {
  it("writes one exact velocity when a bar is dragged", async () => {
    const onSetHitVelocity = vi.fn(async (_hit: DisplayHitBlock, _velocity: number) => null);
    const { container } = mount({ onSetHitVelocity });

    const bar = container.querySelector('[data-hit-index="0"]') as HTMLElement;
    expect(bar).not.toBeNull();

    fireEvent.mouseDown(bar, { clientY: 10, clientX: 2 });
    fireEvent.mouseUp(document);

    expect(onSetHitVelocity).toHaveBeenCalledTimes(1);
    const [hit, velocity] = onSetHitVelocity.mock.calls[0];
    expect(hit.hitIndex).toBe(0);
    expect(velocity).toBeGreaterThanOrEqual(1);
    expect(velocity).toBeLessThanOrEqual(127);
  });

  it("drops the exact velocity with Alt, and only when one is written", async () => {
    const onClearHitVelocity = vi.fn(async (_hit: DisplayHitBlock) => null);
    const { container } = mount({ onClearHitVelocity });

    fireEvent.mouseDown(container.querySelector('[data-hit-index="0"]') as HTMLElement, {
      altKey: true,
      clientY: 10,
    });
    expect(onClearHitVelocity).toHaveBeenCalledTimes(1);

    // The snare follows `mp`; there is no exact value to drop.
    fireEvent.mouseDown(container.querySelector('[data-hit-index="1"]') as HTMLElement, {
      altKey: true,
      clientY: 10,
    });
    expect(onClearHitVelocity).toHaveBeenCalledTimes(1);
  });
});
