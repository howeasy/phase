import { describe, expect, it } from "vitest";

import type { GameEvent, GameObject, GameState, TargetRef } from "../../adapter/types";
import {
  gameObjectFactory,
  buildObjectMap,
} from "../../test/factories/gameObjectFactory";
import { buildGameState } from "../../test/factories/gameStateFactory";
import {
  COMBAT_STRIKE_BANDS,
  combatStrikeDurationBonusMs,
  combatStrikeBandForEvent,
  combatStrikeBandForPower,
  type CombatStrikeBand,
} from "../combatStrike";
import { compileStepsToBeats } from "../compileStepsToBeats";
import { normalizeEvents } from "../eventNormalizer";
import {
  CARD_SLAM_FLIGHT_MS,
  CARD_SLAM_RETURN_MS,
  EVENT_DURATIONS,
  impactDelayMsForAnimationEvent,
  lifeChangeImpactDelayMs,
} from "../types";

const ATTACKER = 1;
const DEFENDER = 2;
const PLAYER = 1;

function creature(id: number, power: number | null): GameObject {
  return gameObjectFactory
    .withId(id)
    .named(`Creature ${id}`)
    .creature(2, 2)
    .onBattlefield()
    .withPower(power)
    .build();
}

function preState(attackerPower: number | null, defenderPower = 1): GameState {
  return buildGameState({
    objects: buildObjectMap(creature(ATTACKER, attackerPower), creature(DEFENDER, defenderPower)),
  });
}

function combatHit(sourceId: number, target: TargetRef, isCombat = true): GameEvent {
  return { type: "DamageDealt", data: { source_id: sourceId, target, amount: 2, is_combat: isCombat } };
}

function lifeChanged(amount = -2, newTotal = 18): GameEvent {
  return { type: "LifeChanged", data: { player_id: PLAYER, amount, new_total: newTotal } };
}

function durationFor(band: CombatStrikeBand): number {
  return EVENT_DURATIONS.DamageDealt
    + (band.flightMs - CARD_SLAM_FLIGHT_MS)
    + (band.returnMs - CARD_SLAM_RETURN_MS);
}

describe("combat strike bands", () => {
  it.each<[number | null, CombatStrikeBand]>([
    [null, COMBAT_STRIKE_BANDS.medium],
    [0, COMBAT_STRIKE_BANDS.light],
    [1, COMBAT_STRIKE_BANDS.light],
    [2, COMBAT_STRIKE_BANDS.medium],
    [3, COMBAT_STRIKE_BANDS.medium],
    [4, COMBAT_STRIKE_BANDS.heavy],
    [7, COMBAT_STRIKE_BANDS.heavy],
    [8, COMBAT_STRIKE_BANDS.colossal],
    [20, COMBAT_STRIKE_BANDS.colossal],
  ])("power %s selects its band", (power, expected) => {
    expect(combatStrikeBandForPower(power)).toEqual(expected);
  });

  it("is only for combat DamageDealt with a pre-event state", () => {
    const state = preState(8);
    expect(combatStrikeBandForEvent(combatHit(ATTACKER, { Player: PLAYER }), state))
      .toEqual(COMBAT_STRIKE_BANDS.colossal);
    expect(combatStrikeBandForEvent(combatHit(ATTACKER, { Player: PLAYER }, false), state)).toBeNull();
    expect(combatStrikeBandForEvent(combatHit(ATTACKER, { Player: PLAYER }), null)).toBeNull();
    // A source the pre-event state does not know (or one with no power) is the
    // 2-3 band, not today's fixed 200.
    expect(combatStrikeBandForEvent(combatHit(99, { Player: PLAYER }), state))
      .toEqual(COMBAT_STRIKE_BANDS.medium);
    expect(
      combatStrikeBandForEvent(
        { type: "GroupedDamageFlurry", data: { player_id: PLAYER, source_ids: [ATTACKER], total_damage: 2, hit_count: 1 } },
        state,
      ),
    ).toBeNull();
  });
});

describe("normalizeEvents combat strike timing", () => {
  it.each<[number, CombatStrikeBand]>([
    [1, COMBAT_STRIKE_BANDS.light],
    [3, COMBAT_STRIKE_BANDS.medium],
    [5, COMBAT_STRIKE_BANDS.heavy],
    [9, COMBAT_STRIKE_BANDS.colossal],
  ])("power %s carries its band, duration and impact", (power, band) => {
    const steps = normalizeEvents([combatHit(ATTACKER, { Player: PLAYER })], {
      preEventState: preState(power),
    });

    expect(steps).toHaveLength(1);
    const effect = steps[0].effects[0];
    expect(effect.strikeBand).toEqual(band);
    expect(effect.duration).toBe(durationFor(band));
    expect(steps[0].duration).toBe(durationFor(band));
    expect(effect.duration - EVENT_DURATIONS.DamageDealt).toBe(
      (band.flightMs - CARD_SLAM_FLIGHT_MS) + (band.returnMs - CARD_SLAM_RETURN_MS),
    );
    expect(impactDelayMsForAnimationEvent(effect.event, null, effect.strikeBand)).toBe(band.flightMs);
    expect(compileStepsToBeats(steps)[0].impactAtMs).toBe(band.flightMs);
  });

  it("keeps a step longer than its impact when combat pacing is shortened", () => {
    const band = COMBAT_STRIKE_BANDS.colossal;
    const [step] = normalizeEvents([combatHit(ATTACKER, { Player: PLAYER })], {
      preEventState: preState(8),
      pacingMultipliers: { effects: 1, combat: 0.25, banners: 1 },
    });

    expect(step.duration).toBe(
      Math.round(EVENT_DURATIONS.DamageDealt * 0.25) + combatStrikeDurationBonusMs(band),
    );
    expect(step.duration).toBeGreaterThan(band.flightMs);
  });

  it.each([0, 0.1, 0.25, 1, 2])("ends no strike step before its impact at combat pacing %s", (combat) => {
    for (const power of [1, 3, 5, 9]) {
      const [step] = normalizeEvents([combatHit(ATTACKER, { Object: DEFENDER })], {
        preEventState: preState(power),
        pacingMultipliers: { effects: 1, combat, banners: 1 },
      });

      expect(step.duration).toBeGreaterThan(step.effects[0].strikeBand?.flightMs ?? Infinity);
    }
  });

  it("paired blocker damage keeps the slamming direction's band and duration", () => {
    const steps = normalizeEvents(
      [
        combatHit(ATTACKER, { Object: DEFENDER }),
        combatHit(DEFENDER, { Object: ATTACKER }),
      ],
      { preEventState: preState(4, 1) },
    );

    expect(steps).toHaveLength(1);
    const [first, second] = steps[0].effects;
    expect(steps[0].effects).toHaveLength(2);
    // Only the 4-power attacker slams; the 1-power blocker's return does not
    // shorten the step or move its impact frame.
    expect(first.strikeBand).toEqual(COMBAT_STRIKE_BANDS.heavy);
    expect(second.strikeBand).toEqual(COMBAT_STRIKE_BANDS.heavy);
    expect(second.duration).toBe(first.duration);
    expect(steps[0].duration).toBe(durationFor(COMBAT_STRIKE_BANDS.heavy));
    expect(impactDelayMsForAnimationEvent(first.event, null, first.strikeBand))
      .toBe(COMBAT_STRIKE_BANDS.heavy.flightMs);
    expect(impactDelayMsForAnimationEvent(second.event, null, second.strikeBand))
      .toBe(COMBAT_STRIKE_BANDS.heavy.flightMs);
  });

  it("leaves non-combat damage exactly as today even with a pre-event state", () => {
    const steps = normalizeEvents(
      [combatHit(ATTACKER, { Player: PLAYER }, false)],
      { preEventState: preState(8) },
    );

    expect(steps[0].effects[0].strikeBand).toBeUndefined();
    expect(steps[0].duration).toBe(EVENT_DURATIONS.DamageDealt);
    expect(impactDelayMsForAnimationEvent(steps[0].effects[0].event)).toBe(CARD_SLAM_FLIGHT_MS);
  });

  it("leaves combat damage with no pre-event state exactly as today", () => {
    const steps = normalizeEvents([combatHit(ATTACKER, { Player: PLAYER })]);

    expect(steps[0].effects[0].strikeBand).toBeUndefined();
    expect(steps[0].duration).toBe(EVENT_DURATIONS.DamageDealt);
    expect(compileStepsToBeats(steps)[0].impactAtMs).toBe(CARD_SLAM_FLIGHT_MS);
  });

  it("makes the life change land with the strike it follows", () => {
    const steps = normalizeEvents(
      [combatHit(ATTACKER, { Player: PLAYER }), lifeChanged()],
      { preEventState: preState(8) },
    );

    const effects = steps[0].effects;
    const lifeEffect = effects.find((effect) => effect.event.type === "LifeChanged");
    expect(lifeEffect).toBeDefined();
    expect(lifeChangeImpactDelayMs(lifeEffect!, effects, PLAYER)).toBe(COMBAT_STRIKE_BANDS.colossal.flightMs);
  });

  it("keeps the flurry's own impact timing (it does not use applyCardSlam)", () => {
    const sources = Array.from({ length: 12 }, (_, index) => index + 1);
    const steps = normalizeEvents(
      sources.flatMap((sourceId) => [combatHit(sourceId, { Player: PLAYER }), lifeChanged()]),
      { preEventState: preState(8) },
    );

    expect(steps).toHaveLength(1);
    expect(steps[0].effects[0].event.type).toBe("GroupedDamageFlurry");
    expect(steps[0].effects[0].strikeBand).toBeUndefined();
  });
});
