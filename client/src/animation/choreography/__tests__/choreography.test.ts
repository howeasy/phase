import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameObject, GameState } from "../../../adapter/types.ts";
import { audioManager } from "../../../audio/AudioManager.ts";
import type { CardVfxLayerHandle } from "../../../components/animation/cardVfx/CardVfxLayer.tsx";
import type { CardVfxSpec } from "../../../components/animation/cardVfx/cardVfxSpecs.ts";
import { useAnimationStore } from "../../../stores/animationStore.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { buildGameObject, buildObjectMap } from "../../../test/factories/gameObjectFactory.ts";
import { buildGameState, buildStackEntry } from "../../../test/factories/gameStateFactory.ts";
import type { StepEffect } from "../../types.ts";
import { createCardVfxPresenter } from "../../presenters/cardVfx.ts";
import type { ChoreographyContext, ClassicPresenter } from "../../presenters/types.ts";
import { presentEffect } from "../index.ts";

const slams = vi.hoisted(() => ({
  impact: null as (() => void) | null,
  knockbacks: 0,
}));

vi.mock("../../../components/animation/CardSlamAnimation.tsx", async (importOriginal) => {
  // This factory is hoisted above the file's imports, so the actual module can
  // only be reached through a dynamic import here.
  const actual = await importOriginal<typeof import("../../../components/animation/CardSlamAnimation.tsx")>();
  return {
    ...actual,
    applyCardSlam: vi.fn((
      _element: HTMLElement,
      _targetX: number,
      _targetY: number,
      _speedMultiplier: number,
      onImpact: () => void,
    ) => {
      slams.impact = onImpact;
      return true;
    }),
    applyCardKnockback: vi.fn(() => {
      slams.knockbacks += 1;
      return true;
    }),
  };
});

const X = 7;
const Y = 8;
const TOKEN = 9;

function boardObject(id: number, zone: GameObject["zone"] = "Battlefield"): GameObject {
  return buildGameObject({
    id,
    name: `Card ${id}`,
    zone,
    display_visible_to_viewer: true,
  });
}

function gameState(...objects: GameObject[]): GameState {
  return buildGameState({ objects: buildObjectMap(...objects) });
}

function seed(pre: GameState | null, post: GameState | null): void {
  useGameStore.setState({ gameState: pre });
  useAnimationStore.getState().setAnimationNewState(post);
}

function rect(): DOMRect {
  return new DOMRect(10, 20, 63, 88);
}

/** Records every visible Classic call; timers run so a scheduled impact is
 *  recorded where it lands, not when it was scheduled. */
function recordingClassic(
  calls: string[],
  cardElement: (objectId: number) => HTMLElement | null = () => null,
): ClassicPresenter {
  return {
    spawnFloat: (_position, value, color) => calls.push(`float:${value}:${color}`),
    spawnDeathClone: () => calls.push("deathClone"),
    spawnReveal: () => calls.push("reveal"),
    spawnCastArc: (_from, _to, _snapshot, mode) => calls.push(`castArc:${mode}`),
    spawnMillReveal: () => calls.push("millReveal"),
    spawnRippleReveal: () => calls.push("rippleReveal"),
    spawnMeldForge: () => calls.push("meldForge"),
    queuePendingDeath: () => calls.push("pendingDeath"),
    showVignette: (amount) => calls.push(`vignette:${amount}`),
    clearVignette: () => calls.push("clearVignette"),
    landDamageHit: (_position, amount, isPlayerTarget) =>
      calls.push(`landDamageHit:${amount}:${isPlayerTarget}`),
    particles: {
      explosion: () => calls.push("particles.explosion"),
      spellImpact: () => calls.push("particles.spellImpact"),
      playerDamage: () => calls.push("particles.playerDamage"),
      healEffect: () => calls.push("particles.healEffect"),
      summonBurst: () => calls.push("particles.summonBurst"),
      slamImpact: () => calls.push("particles.slamImpact"),
      forgeStrike: () => calls.push("particles.forgeStrike"),
      forgeHeat: () => calls.push("particles.forgeHeat"),
      damageFlurry: () => calls.push("particles.damageFlurry"),
    },
    screenShake: (intensity) => calls.push(`shake:${intensity}`),
    objectRect: () => rect(),
    snapshotRect: () => rect(),
    registryRect: () => rect(),
    objectPosition: () => ({ x: 50, y: 60 }),
    playerHudPosition: () => ({ x: 100, y: 120 }),
    damageTargetPosition: () => ({ x: 200, y: 220 }),
    center: (value) => ({ x: value.x + value.width / 2, y: value.y + value.height / 2 }),
    cardElement,
    pileRect: () => null,
    scheduleStepTimeout: (callback, delay) => {
      setTimeout(callback, delay);
    },
  };
}

type LayerMode = "accept" | "decline";

function layerFor(mode: LayerMode) {
  const specs: CardVfxSpec[] = [];
  const handle: CardVfxLayerHandle = {
    present(spec, classic, onImpact) {
      specs.push(spec);
      // An accepted effect's impact is the layer's to time; land it at once.
      if (mode === "decline") classic();
      else onImpact?.();
    },
  };
  return { handle, specs };
}

interface RunOptions {
  cardElement?: (objectId: number) => HTMLElement | null;
}

function run(effect: StepEffect, mode: "none" | "decline" | "accept", options: RunOptions = {}) {
  const calls: string[] = [];
  const classic = recordingClassic(calls, options.cardElement);
  const layer = mode === "none" ? null : layerFor(mode);
  slams.impact = null;
  slams.knockbacks = 0;
  const sfx = vi.spyOn(audioManager, "playSfx").mockImplementation((type, volume = 1) => {
    calls.push(`sfx:${type}:${volume}`);
  });
  const context: ChoreographyContext = {
    stepEffects: [effect],
    owningStepMs: 500,
    snapshotSeq: 1,
    pace: 1,
    vfxQuality: "full",
    reduceMotion: false,
    classic,
    cardVfx: createCardVfxPresenter(() => layer?.handle ?? null),
  };
  try {
    presentEffect(effect, context);
    // `presentEffect` assigns through the mock, which control-flow narrowing
    // from the reset above cannot see.
    const impact = slams.impact as (() => void) | null;
    slams.impact = null;
    impact?.();
    vi.runAllTimers();
    return { calls, specs: layer?.specs ?? [], knockbacks: slams.knockbacks };
  } finally {
    sfx.mockRestore();
  }
}

interface Case {
  name: string;
  effect: StepEffect;
  seed?: () => void;
  cardElement?: () => HTMLElement;
  /** Specs `cardVfxSpecFor` or the choreography offers an accepting layer. */
  specKinds: CardVfxSpec["kind"][];
  none: string[];
  accept: string[];
  knockbacks?: { none: number; decline: number; accept: number };
}

function effectFor(event: StepEffect["event"]): StepEffect {
  return { event, duration: 500 };
}

const flurry = effectFor({
  type: "GroupedDamageFlurry",
  data: { player_id: 1, source_ids: [X], total_damage: 9, hit_count: 3 },
});

const damageSource = boardObject(X);
const damageTarget = boardObject(Y);
const damageDealt = effectFor({
  type: "DamageDealt",
  data: { source_id: X, target: { Object: Y }, amount: 3, is_combat: false },
});
const slamAtCreature = effectFor({
  type: "DamageDealt",
  data: { source_id: X, target: { Object: Y }, amount: 3, is_combat: true },
});
const slamAtPlayer = effectFor({
  type: "DamageDealt",
  data: { source_id: X, target: { Player: 1 }, amount: 3, is_combat: true },
});

const lifeChanged = effectFor({ type: "LifeChanged", data: { player_id: 1, amount: 3 } });

const destroyed = effectFor({ type: "CreatureDestroyed", data: { object_id: X, source_id: null } });
const sacrificed = effectFor({ type: "PermanentSacrificed", data: { object_id: X, player_id: 0 } });

const spellCast = effectFor({
  type: "SpellCast",
  data: { card_id: X, controller: 0, object_id: X },
});

const turnStarted = effectFor({ type: "TurnStarted", data: { player_id: 0, turn_number: 1 } });

const zoneEntered = effectFor({
  type: "ZoneChanged",
  data: { object_id: X, from: "Stack", to: "Battlefield" },
});

const melded = effectFor({
  type: "Melded",
  data: { object_id: X, partner_id: Y, controller: 0 },
});

const tokenCreated = effectFor({
  type: "TokenCreated",
  data: { object_id: TOKEN, name: "Soldier", source_id: X },
});

const cardsRevealed = effectFor({
  type: "CardsRevealed",
  data: { player: 0, card_ids: [10], card_names: ["Opt"] },
});

const meldCalls = [
  "meldForge",
  "particles.forgeHeat",
  "sfx:DamageDealt:0.7",
  "particles.forgeStrike",
  "shake:light",
  "sfx:DamageDealt:0.7",
  "particles.forgeStrike",
  "shake:light",
  "sfx:DamageDealt:1",
  "particles.forgeStrike",
  "shake:medium",
  "particles.summonBurst",
];

const cases: Case[] = [
  {
    name: "GroupedDamageFlurry",
    effect: flurry,
    seed: () => seed(gameState(damageSource), gameState(damageSource)),
    specKinds: ["blow"],
    none: [
      "particles.damageFlurry",
      "sfx:DamageDealt:1",
      "float:-9:#ef4444",
      "particles.playerDamage",
      "vignette:9",
      "shake:light",
      "clearVignette",
    ],
    accept: [
      "particles.damageFlurry",
      "sfx:DamageDealt:1",
      "float:-9:#ef4444",
      "vignette:9",
      "shake:light",
      "clearVignette",
    ],
  },
  {
    name: "DamageDealt (damage cause)",
    effect: damageDealt,
    seed: () =>
      seed(
        buildGameState({
          objects: buildObjectMap(damageSource, damageTarget),
          stack: [buildStackEntry({ id: 30, source_id: X })],
        }),
        gameState(damageSource, damageTarget),
      ),
    specKinds: ["damage"],
    none: ["sfx:DamageDealt:1", "float:-3:#ef4444"],
    accept: ["landDamageHit:3:false"],
  },
  {
    name: "DamageDealt (creature-to-creature slam)",
    effect: slamAtCreature,
    seed: () => seed(gameState(damageSource, damageTarget), gameState(damageSource, damageTarget)),
    cardElement: () => document.createElement("div"),
    specKinds: ["blow", "knockback"],
    none: ["particles.slamImpact", "landDamageHit:3:false"],
    accept: ["landDamageHit:3:false"],
    knockbacks: { none: 0, decline: 1, accept: 0 },
  },
  {
    name: "DamageDealt (creature-to-player slam)",
    effect: slamAtPlayer,
    seed: () => seed(gameState(damageSource), gameState(damageSource)),
    cardElement: () => document.createElement("div"),
    specKinds: ["blow"],
    none: ["particles.playerDamage", "landDamageHit:3:true"],
    accept: ["landDamageHit:3:true"],
  },
  {
    name: "LifeChanged",
    effect: lifeChanged,
    seed: () => seed(gameState(), gameState()),
    specKinds: ["life"],
    none: ["float:3:#22c55e", "particles.healEffect"],
    accept: ["float:3:#22c55e"],
  },
  {
    name: "CreatureDestroyed",
    effect: destroyed,
    seed: () => seed(gameState(boardObject(X)), gameState()),
    specKinds: ["shatter"],
    none: ["particles.explosion", "pendingDeath"],
    accept: [],
  },
  {
    name: "PermanentSacrificed",
    effect: sacrificed,
    seed: () => seed(gameState(boardObject(X)), gameState(boardObject(X))),
    specKinds: ["dissolve"],
    none: ["particles.explosion", "deathClone"],
    accept: [],
  },
  {
    name: "SpellCast",
    effect: spellCast,
    seed: () =>
      seed(gameState(boardObject(X, "Hand")), gameState(boardObject(X, "Stack"))),
    specKinds: ["flight"],
    none: ["particles.spellImpact", "castArc:cast"],
    accept: [],
  },
  {
    name: "TurnStarted",
    effect: turnStarted,
    specKinds: [],
    none: [],
    accept: [],
  },
  {
    name: "ZoneChanged",
    effect: zoneEntered,
    seed: () => seed(gameState(boardObject(X, "Stack")), gameState(boardObject(X))),
    specKinds: ["flight"],
    none: ["reveal", "particles.summonBurst", "castArc:resolve-permanent"],
    accept: [],
  },
  {
    name: "Melded",
    effect: melded,
    seed: () => seed(gameState(boardObject(X), boardObject(Y)), gameState(boardObject(X))),
    specKinds: [],
    none: meldCalls,
    accept: meldCalls,
  },
  {
    name: "TokenCreated",
    effect: tokenCreated,
    seed: () => seed(gameState(boardObject(X)), gameState(boardObject(TOKEN))),
    specKinds: ["flight"],
    none: ["reveal", "particles.summonBurst"],
    accept: [],
  },
  {
    name: "CardsRevealed",
    effect: cardsRevealed,
    specKinds: [],
    none: ["rippleReveal"],
    accept: ["rippleReveal"],
  },
];

beforeEach(() => {
  vi.useFakeTimers();
  useGameStore.getState().reset();
  useAnimationStore.getState().clearQueue();
  useAnimationStore.getState().setAnimationNewState(null);
});

afterEach(() => {
  useAnimationStore.getState().clearQueue();
  useAnimationStore.getState().setAnimationNewState(null);
  useGameStore.getState().reset();
  vi.clearAllMocks();
  vi.useRealTimers();
});

describe("event choreographies under both presenters", () => {
  for (const testCase of cases) {
    it(`${testCase.name}: a declining layer records the Classic calls`, () => {
      testCase.seed?.();

      const none = run(testCase.effect, "none", { cardElement: testCase.cardElement });
      const decline = run(testCase.effect, "decline", { cardElement: testCase.cardElement });

      expect(none.specs).toEqual([]);
      expect(none.calls).toEqual(testCase.none);
      expect(decline.calls).toEqual(testCase.none);
      expect(none.knockbacks).toBe(testCase.knockbacks?.none ?? 0);
      expect(decline.knockbacks).toBe(testCase.knockbacks?.decline ?? 0);
    });

    it(`${testCase.name}: an accepting layer presents exactly its spec kinds`, () => {
      testCase.seed?.();

      const accept = run(testCase.effect, "accept", { cardElement: testCase.cardElement });

      expect(accept.specs.map((spec) => spec.kind)).toEqual(testCase.specKinds);
      expect(accept.calls).toEqual(testCase.accept);
      expect(accept.knockbacks).toBe(testCase.knockbacks?.accept ?? 0);
    });
  }
});
