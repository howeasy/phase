import type { GameState } from "../adapter/types";
import type { AnimationEvent } from "./types";
import { CARD_SLAM_FLIGHT_MS, CARD_SLAM_RETURN_MS } from "./types";

/**
 * One combat strike's timing. All values are base milliseconds, before the
 * presentation pace multiplier. `hang` winds the card back for the first 25%
 * of the flight, holds it, then accelerates into the target; without it the
 * flight keeps today's curve.
 */
export interface CombatStrikeBand {
  flightMs: number;
  returnMs: number;
  hang: boolean;
}

/**
 * The one authority for combat strike bands, by the striking creature's power
 * in the pre-event state.
 *
 * The first band is today's fixed slam (`CARD_SLAM_FLIGHT_MS`/`_RETURN_MS`);
 * the others grow with power. Unknown or missing power uses `medium`, the
 * 2-3 band.
 */
export const COMBAT_STRIKE_BANDS = {
  /** Power <= 1: today's slam. */
  light: { flightMs: CARD_SLAM_FLIGHT_MS, returnMs: CARD_SLAM_RETURN_MS, hang: false },
  /** Power 2-3, and unknown power. */
  medium: { flightMs: 230, returnMs: 250, hang: false },
  /** Power 4-7. */
  heavy: { flightMs: 320, returnMs: 380, hang: true },
  /** Power 8+. */
  colossal: { flightMs: 420, returnMs: 500, hang: true },
} as const satisfies Record<string, CombatStrikeBand>;

/** The band for a striking creature's power; `null`/unknown uses 2-3. */
export function combatStrikeBandForPower(power: number | null | undefined): CombatStrikeBand {
  if (power == null) return COMBAT_STRIKE_BANDS.medium;
  if (power <= 1) return COMBAT_STRIKE_BANDS.light;
  if (power <= 3) return COMBAT_STRIKE_BANDS.medium;
  if (power <= 7) return COMBAT_STRIKE_BANDS.heavy;
  return COMBAT_STRIKE_BANDS.colossal;
}

/**
 * The band for `event`, read from the state BEFORE its batch: only combat
 * `DamageDealt` has one, and only when the caller supplied that state. Callers
 * with no pre-event state (tests, previews) keep today's fixed slam.
 */
export function combatStrikeBandForEvent(
  event: AnimationEvent,
  preEventState: GameState | null | undefined,
): CombatStrikeBand | null {
  if (!preEventState || event.type !== "DamageDealt" || !event.data.is_combat) return null;
  return combatStrikeBandForPower(preEventState.objects[String(event.data.source_id)]?.power);
}

/** How far past its impact a strike's step must reach, so the hit's own cues
 *  (scheduled on the step) are never cancelled by a short combat pacing. */
const STRIKE_SETTLE_FLOOR_MS = 80;

/**
 * A strike step's duration before pace. `pacedBaseMs` is the base duration under
 * the combat pacing setting; the card slam does not follow that setting, so
 * neither does the time a heavier strike adds, and the step never ends before
 * the blow has landed.
 */
export function combatStrikeStepMs(band: CombatStrikeBand, pacedBaseMs: number): number {
  return Math.max(pacedBaseMs + combatStrikeDurationBonusMs(band), band.flightMs + STRIKE_SETTLE_FLOOR_MS);
}

/** The extra step duration a band adds, before pace: flight plus return growth. */
export function combatStrikeDurationBonusMs(band: CombatStrikeBand): number {
  return (band.flightMs - CARD_SLAM_FLIGHT_MS) + (band.returnMs - CARD_SLAM_RETURN_MS);
}
