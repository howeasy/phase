import { act, cleanup, render } from "@testing-library/react";
import type * as Motion from "framer-motion";
import type { HTMLAttributes } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, GameState } from "../../../adapter/types.ts";
import { ATTACK_LUNGE_BANDS } from "../../../animation/attackLunge.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { useUiStore } from "../../../stores/uiStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildPriorityWaitingFor } from "../../../test/factories/gameStateFactory.ts";
import { BoardInteractionContext } from "../BoardInteractionContext.tsx";
import { PermanentCard } from "../PermanentCard.tsx";

interface CapturedCard {
  id: number;
  animate?: Record<string, unknown>;
  transition?: Record<string, unknown>;
}

const motionState = vi.hoisted(() => ({
  reduced: false,
  cards: [] as CapturedCard[],
}));

vi.mock("framer-motion", async (importOriginal) => {
  const actual = await importOriginal<typeof Motion>();
  // `vi.mock` is hoisted above the imports, so React is loaded inside the
  // factory rather than through a binding that may not be initialized yet.
  const react = await import("react");

  const MockDiv = react.forwardRef<HTMLDivElement, Record<string, unknown>>((props, ref) => {
    const rawId = props["data-object-id"];
    if (typeof rawId === "number" || typeof rawId === "string") {
      const animate = props.animate;
      const transition = props.transition;
      motionState.cards.push({
        id: Number(rawId),
        animate:
          typeof animate === "object" && animate !== null
            ? (animate as Record<string, unknown>)
            : undefined,
        transition:
          typeof transition === "object" && transition !== null
            ? (transition as Record<string, unknown>)
            : undefined,
      });
    }
    const rest: Record<string, unknown> = { ...props };
    for (const key of [
      "animate",
      "transition",
      "initial",
      "exit",
      "variants",
      "layoutId",
      "whileHover",
      "whileTap",
    ]) {
      delete rest[key];
    }
    return <div {...(rest as HTMLAttributes<HTMLDivElement>)} ref={ref} />;
  });

  return {
    ...actual,
    motion: new Proxy(actual.motion, {
      get(target, property, receiver) {
        return property === "div" ? MockDiv : Reflect.get(target, property, receiver);
      },
    }),
    useReducedMotion: () => motionState.reduced,
  };
});

vi.mock("../../card/CardImage.tsx", () => ({
  CardImage: () => <div />,
}));

const OBJECT = 1;
const ATTACK_SLIDE = -30;

function makeObject(power: number | null = 2): GameObject {
  return buildGameObject({
    id: OBJECT,
    card_id: 100,
    zone: "Battlefield",
    name: "Grizzly Bears",
    power,
    toughness: 2,
    base_power: power,
    base_toughness: 2,
    color: ["Green"],
    base_color: ["Green"],
    entered_battlefield_turn: null,
    card_types: { supertypes: [], core_types: ["Creature"], subtypes: [] },
  });
}

function makeState(power: number | null = 2): GameState {
  return buildGameState({
    objects: buildObjectMap(makeObject(power)),
    battlefield: [OBJECT],
    stack: [],
    waiting_for: buildPriorityWaitingFor(),
  });
}

function setPower(power: number | null): void {
  const gameState = makeState(power);
  useGameStore.setState({ gameState, waitingFor: gameState.waiting_for });
}

function boardValue(committed: number[]) {
  return {
    activatableObjectIds: new Set<number>(),
    blockableAttackerIds: new Set<number>(),
    boardChoiceObjectIds: new Set<number>(),
    committedAttackerIds: new Set(committed),
    incomingAttackerCounts: new Map<number, number>(),
    manaTappableObjectIds: new Set<number>(),
    selectableSacrificeObjectIds: new Set<number>(),
    selectableManaCostCreatureIds: new Set<number>(),
    undoableTapObjectIds: new Set<number>(),
    validAttackerIds: new Set<number>(),
    validTargetObjectIds: new Set<number>(),
  };
}

function view(committed: number[]) {
  return (
    <BoardInteractionContext.Provider value={boardValue(committed)}>
      <PermanentCard objectId={OBJECT} />
    </BoardInteractionContext.Provider>
  );
}

function lastCard(): CapturedCard {
  const visible = motionState.cards.filter((card) => card.id === OBJECT);
  const last = visible[visible.length - 1];
  if (!last) throw new Error("card was never rendered");
  return last;
}

/** The captured keyframe spec for `y`, or `{}` when it is not a config object. */
function yTransition(card: CapturedCard): Record<string, unknown> {
  const y = card.transition?.y;
  return typeof y === "object" && y !== null ? (y as Record<string, unknown>) : {};
}

function isNumberArray(value: unknown): value is number[] {
  return Array.isArray(value) && value.every((frame) => typeof frame === "number");
}

/** The numeric keyframe list captured for a value, or `[]` when it is not one. */
function frames(value: unknown): number[] {
  return isNumberArray(value) ? value : [];
}

function useMatchMedia() {
  window.matchMedia = ((query: string) => ({
    matches: query === "(hover: hover)" || query === "(any-hover: hover)",
    media: query,
    onchange: null,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
    addListener: vi.fn(),
    removeListener: vi.fn(),
    dispatchEvent: vi.fn(),
  })) as unknown as typeof window.matchMedia;
}

const innerHeightDescriptor = Object.getOwnPropertyDescriptor(window, "innerHeight");

function setInnerHeight(value: number) {
  Object.defineProperty(window, "innerHeight", { configurable: true, value });
}

beforeEach(() => {
  motionState.reduced = false;
  motionState.cards.length = 0;
  useMatchMedia();
  setPower(2);
  useUiStore.setState({
    selectedObjectId: null,
    hoveredObjectId: null,
    inspectedObjectId: null,
    combatMode: null,
    selectedAttackers: [],
    blockerAssignments: new Map(),
    combatClickHandler: null,
    selectedCardIds: [],
    pendingAbilityChoice: null,
  });
  usePreferencesStore.setState({
    battlefieldCardDisplay: "full_card",
    showKeywordStrip: false,
    tapRotation: "classic",
    animationSpeedMultiplier: 1,
  });
});

afterEach(() => {
  cleanup();
  useGameStore.getState().reset();
  if (innerHeightDescriptor) Object.defineProperty(window, "innerHeight", innerHeightDescriptor);
  else Reflect.deleteProperty(window, "innerHeight");
});

describe("PermanentCard committed-attacker lunge", () => {
  it("publishes the four power bands as the single numeric authority", () => {
    expect(
      ATTACK_LUNGE_BANDS.map((band) => [
        band.maxPower,
        band.durationMs,
        band.windUp,
        band.thrust,
        band.peakScale,
        band.hang,
      ]),
    ).toEqual([
      [1, 360, 0.25, 1.6, 1.04, 0],
      [3, 460, 0.4, 1.9, 1.08, 0],
      [7, 640, 0.55, 2.15, 1.11, 0.25],
      [null, 780, 0.65, 2.35, 1.14, 0.3],
    ]);
  });

  it.each([
    { power: 1, band: 0 },
    { power: 3, band: 1 },
    { power: 4, band: 2 },
    { power: 8, band: 3 },
  ])("power $power takes duration and peak scale from band $band", ({ power, band }) => {
    setPower(power);
    const { rerender } = render(view([]));

    rerender(view([OBJECT]));

    const spec = ATTACK_LUNGE_BANDS[band];
    const animate = lastCard().animate;
    const played = frames(animate?.y);
    expect(yTransition(lastCard()).duration).toBeCloseTo(spec.durationMs / 1000);
    expect(Math.max(...frames(animate?.scale))).toBeCloseTo(spec.peakScale);
    expect(played[1]).toBeCloseTo(30 * spec.windUp);
    expect(played[played.length - 2]).toBeCloseTo(-30 * spec.thrust);
    expect(played[played.length - 1]).toBe(ATTACK_SLIDE);
  });

  it.each([
    { power: 1, band: 0, hanging: false },
    { power: 3, band: 1, hanging: false },
    { power: 4, band: 2, hanging: true },
    { power: 8, band: 3, hanging: true },
  ])("power $power hangs at the wind-up only from power 4", ({ power, band, hanging }) => {
    setPower(power);
    const { rerender } = render(view([]));

    rerender(view([OBJECT]));

    const played = frames(lastCard().animate?.y);
    const times = frames(yTransition(lastCard()).times);
    if (hanging) {
      expect(played).toHaveLength(5);
      expect(played[1]).toBe(played[2]);
      expect(times).toHaveLength(5);
      expect(times[2] - times[1]).toBeCloseTo(ATTACK_LUNGE_BANDS[band].hang);
      expect(times[3] - times[2]).toBeLessThan(times[2] - times[1]);
    } else {
      expect(played).toHaveLength(4);
      expect(played[1]).not.toBe(played[2]);
      expect(times).toHaveLength(4);
    }
  });

  it("reads a null power as the 2–3 band", () => {
    setPower(null);
    const { rerender } = render(view([]));

    rerender(view([OBJECT]));

    const standard = ATTACK_LUNGE_BANDS[1];
    expect(yTransition(lastCard()).duration).toBeCloseTo(standard.durationMs / 1000);
    expect(Math.max(...frames(lastCard().animate?.scale))).toBeCloseTo(standard.peakScale);
  });

  it("does not restart when power changes mid-lunge", () => {
    setPower(2);
    const { rerender } = render(view([]));
    rerender(view([OBJECT]));
    const played = lastCard().animate?.y;
    expect(frames(played)).toHaveLength(4);
    expect(yTransition(lastCard()).duration).toBeCloseTo(ATTACK_LUNGE_BANDS[1].durationMs / 1000);

    const stronger = makeState(8);
    act(() => {
      useGameStore.setState({ gameState: stronger, waitingFor: stronger.waiting_for });
    });

    expect(lastCard().animate?.y).toBe(played);
    expect(frames(played)).toHaveLength(4);
    expect(yTransition(lastCard()).duration).toBeCloseTo(ATTACK_LUNGE_BANDS[1].durationMs / 1000);
  });

  it("does not lunge when it mounts already attacking", () => {
    render(view([OBJECT]));

    const animate = lastCard().animate;
    expect(animate?.y).toBe(ATTACK_SLIDE);
    expect(frames(animate?.y)).toHaveLength(0);
  });

  it("lunges a card remounted by the update that declared it attacking", () => {
    useGameStore.setState({
      events: [{ type: "AttackersDeclared", data: { attacker_ids: [OBJECT], defending_player: 1 } }],
    });

    render(view([OBJECT]));

    expect(frames(lastCard().animate?.y)).toHaveLength(4);
  });

  it.each([
    { mountedFirst: false, how: "remounted card" },
    { mountedFirst: true, how: "card already on the board" },
  ])("does not replay a declaration after a $how has lunged", ({ mountedFirst }) => {
    const first = render(view([]));
    if (!mountedFirst) first.unmount();
    useGameStore.setState({
      events: [{ type: "AttackersDeclared", data: { attacker_ids: [OBJECT], defending_player: 1 } }],
    });
    const played = mountedFirst ? first : render(view([OBJECT]));
    if (mountedFirst) played.rerender(view([OBJECT]));
    expect(frames(lastCard().animate?.y)).toHaveLength(4);
    played.unmount();

    render(view([OBJECT]));

    expect(lastCard().animate?.y).toBe(ATTACK_SLIDE);
  });

  it("does not replay a hidden member when its collapsed group expands", () => {
    const stoodInFor = 2;
    useGameStore.setState({
      events: [
        { type: "AttackersDeclared", data: { attacker_ids: [stoodInFor, OBJECT], defending_player: 1 } },
      ],
    });
    // The collapsed group renders one representative covering both members.
    const collapsed = render(
      <BoardInteractionContext.Provider value={boardValue([stoodInFor, OBJECT])}>
        <PermanentCard objectId={stoodInFor} coveredIds={[stoodInFor, OBJECT]} />
      </BoardInteractionContext.Provider>,
    );
    collapsed.unmount();

    render(view([stoodInFor, OBJECT]));

    expect(lastCard().animate?.y).toBe(ATTACK_SLIDE);
  });

  it("does not lunge when a new session restores an attack", () => {
    const { rerender } = render(view([]));

    act(() => {
      useGameStore.setState({
        gameSessionGeneration: useGameStore.getState().gameSessionGeneration + 1,
      });
      rerender(view([OBJECT]));
    });

    expect(lastCard().animate?.y).toBe(ATTACK_SLIDE);
  });

  it("plays a wind-up and thrust when the attacker is committed", () => {
    const { rerender } = render(view([]));
    expect(lastCard().animate?.y).toBe(0);

    rerender(view([OBJECT]));

    const animate = lastCard().animate;
    expect(frames(animate?.y)).toEqual([0, 12, -57, ATTACK_SLIDE]);
    expect(frames(animate?.scale)).toEqual([1, 1, ATTACK_LUNGE_BANDS[1].peakScale, 1]);
    expect(lastCard().transition).toMatchObject({
      rotate: { type: "spring" },
      opacity: { type: "spring" },
      y: { duration: ATTACK_LUNGE_BANDS[1].durationMs / 1000 },
      scale: { duration: ATTACK_LUNGE_BANDS[1].durationMs / 1000 },
    });
  });

  it("thrusts a selected, already slid attacker no further than an unselected one", () => {
    // Selecting slid the card forward, so the commit starts from there.
    const computed = vi
      .spyOn(window, "getComputedStyle")
      .mockReturnValue({ transform: `matrix(1, 0, 0, 1, 0, ${ATTACK_SLIDE})` } as CSSStyleDeclaration);
    const { rerender } = render(view([]));

    rerender(view([OBJECT]));

    const spec = ATTACK_LUNGE_BANDS[1];
    const played = frames(lastCard().animate?.y);
    expect(played[0]).toBe(ATTACK_SLIDE);
    expect(played[1]).toBeCloseTo(ATTACK_SLIDE + 30 * spec.windUp);
    expect(played[2]).toBeCloseTo(ATTACK_SLIDE * spec.thrust);
    computed.mockRestore();
  });

  it("never lengthens the lunge past the multiplier 1 duration", () => {
    usePreferencesStore.setState({ animationSpeedMultiplier: 2 });
    const { rerender } = render(view([]));

    rerender(view([OBJECT]));

    expect(yTransition(lastCard()).duration).toBeCloseTo(ATTACK_LUNGE_BANDS[1].durationMs / 1000);
  });

  it("keeps today's plain slide while attackers are being selected", () => {
    useUiStore.setState({ combatMode: "attackers", selectedAttackers: [OBJECT] });

    render(view([]));

    expect(lastCard().animate?.y).toBe(ATTACK_SLIDE);
    expect(frames(lastCard().animate?.y)).toHaveLength(0);
  });

  it("keeps today's plain slide at speed 0", () => {
    usePreferencesStore.setState({ animationSpeedMultiplier: 0 });
    const { rerender } = render(view([]));

    rerender(view([OBJECT]));

    expect(lastCard().animate?.y).toBe(ATTACK_SLIDE);
    expect(frames(lastCard().animate?.y)).toHaveLength(0);
  });

  it("keeps today's plain slide under reduced motion", () => {
    motionState.reduced = true;
    const { rerender } = render(view([]));

    rerender(view([OBJECT]));

    expect(lastCard().animate?.y).toBe(ATTACK_SLIDE);
    expect(frames(lastCard().animate?.y)).toHaveLength(0);
  });

  it("scales the compact-height lunge with its smaller slide", () => {
    setInnerHeight(400);
    const { rerender } = render(view([]));

    rerender(view([OBJECT]));

    const played = frames(lastCard().animate?.y);
    expect(played).toHaveLength(4);
    expect(played[1]).toBeCloseTo(4.8);
    expect(played[2]).toBeCloseTo(-22.8);
    expect(played[3]).toBe(-12);
  });

  it("does not restart the lunge on a re-render", () => {
    const { rerender } = render(view([]));
    rerender(view([OBJECT]));
    const played = lastCard().animate?.y;
    expect(frames(played)).toHaveLength(4);

    rerender(view([OBJECT]));

    expect(lastCard().animate?.y).toBe(played);
  });
});
