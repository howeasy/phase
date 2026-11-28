import { act, cleanup, render } from "@testing-library/react";
import type { RefObject } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { AnimationStep } from "../../../animation/types.ts";
import { currentSnapshot } from "../../../hooks/useGameDispatch.ts";
import { useAnimationStore } from "../../../stores/animationStore.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { buildObjectMap, gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState } from "../../../test/factories/gameStateFactory.ts";
import { AnimationOverlay } from "../AnimationOverlay.tsx";
import type { ParticleCanvasHandle } from "../ParticleCanvas.tsx";

const particles = vi.hoisted(() => ({
  explosion: vi.fn(),
  projectile: vi.fn(),
  spellImpact: vi.fn(),
  damageFlash: vi.fn(),
  playerDamage: vi.fn(),
  healEffect: vi.fn(),
  summonBurst: vi.fn(),
  blockClash: vi.fn(),
  attackBurst: vi.fn(),
  slamImpact: vi.fn(),
  forgeStrike: vi.fn(),
  forgeHeat: vi.fn(),
  damageFlurry: vi.fn(),
}) satisfies ParticleCanvasHandle);

vi.mock("../ParticleCanvas.tsx", async () => {
  // This factory is hoisted above the file's imports, so React can only be
  // reached through a dynamic import here.
  const { forwardRef, useImperativeHandle } = await import("react");
  return {
    ParticleCanvas: forwardRef<ParticleCanvasHandle>((_props, ref) => {
      useImperativeHandle(ref, () => particles);
      return null;
    }),
  };
});

vi.mock("framer-motion", async (importOriginal) => ({
  ...(await importOriginal<typeof import("framer-motion")>()),
  useReducedMotion: () => false,
}));

const X = 7;
const containerRef = { current: null } as RefObject<HTMLDivElement | null>;
const elves = gameObjectFactory.withId(X).named("Llanowar Elves").creature(1, 1);

function seedCast() {
  currentSnapshot.set(X, new DOMRect(40, 600, 63, 88));
  act(() => {
    useGameStore.setState({
      gameState: buildGameState({ objects: buildObjectMap(elves.inHand().build()) }),
    });
    useAnimationStore.getState().setAnimationNewState(
      buildGameState({
        objects: buildObjectMap(elves.params({ zone: "Stack" }).build()),
      }),
    );
    const event: AnimationStep["effects"][number]["event"] = {
      type: "SpellCast",
      data: { card_id: X, controller: 0, object_id: X },
    };
    useAnimationStore
      .getState()
      .enqueueSteps([{ effects: [{ event, duration: 500 }], duration: 500 }], 1);
  });
}

beforeEach(() => {
  currentSnapshot.clear();
  usePreferencesStore.setState({
    cardAnimationStyle: "classic",
    vfxQuality: "full",
    animationSpeedMultiplier: 1,
  });
});

afterEach(() => {
  cleanup();
  useAnimationStore.getState().clearQueue();
  useGameStore.getState().reset();
  currentSnapshot.clear();
  vi.clearAllMocks();
});

describe("AnimationOverlay re-render", () => {
  it("does not present an active step again when the overlay re-renders", () => {
    seedCast();

    const { rerender } = render(<AnimationOverlay containerRef={containerRef} />);
    expect(useAnimationStore.getState().activeStep).not.toBeNull();
    expect(particles.spellImpact).toHaveBeenCalledTimes(1);

    rerender(<AnimationOverlay containerRef={containerRef} />);

    expect(particles.spellImpact).toHaveBeenCalledTimes(1);
  });
});
