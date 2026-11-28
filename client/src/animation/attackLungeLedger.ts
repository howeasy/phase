import type { Easing } from "framer-motion";

import type { ObjectId } from "../adapter/types.ts";

/** One card's lunge, frozen when it starts: later changes cannot reshape it. */
export interface CardLunge {
  yKeyframes: number[];
  scaleKeyframes: number[];
  times: number[];
  ease: Easing | Easing[];
  durationMs: number;
  /** Already played when a redrawn card picks the lunge up. */
  elapsedMs: number;
}

/**
 * What an attacker has played for its current attack. It outlives the card
 * component, because choosing or declaring attackers can redraw a card (a group
 * splitting, expanding or collapsing) while its lunge is running.
 */
export interface LungeRecord {
  generation: number;
  startedAt: number;
  /** `null` when the attack is spent without motion of its own. */
  lunge: CardLunge | null;
}

const records = new Map<ObjectId, LungeRecord>();

/** Drops every record whose creature is no longer chosen or declared. */
export function forgetEndedAttacks(isAttacking: (id: ObjectId) => boolean): void {
  for (const id of records.keys()) {
    if (!isAttacking(id)) records.delete(id);
  }
}

export function lungeRecord(id: ObjectId, generation: number): LungeRecord | undefined {
  const record = records.get(id);
  return record?.generation === generation ? record : undefined;
}

export function recordLunge(id: ObjectId, record: LungeRecord): void {
  records.set(id, record);
}

/** The rest of a recorded lunge, or `null` once it has run out. */
export function remainingLunge(record: LungeRecord, now: number): CardLunge | null {
  if (!record.lunge) return null;
  const elapsedMs = Math.max(0, now - record.startedAt);
  return elapsedMs < record.lunge.durationMs ? { ...record.lunge, elapsedMs } : null;
}

export function resetLungeLedger(): void {
  records.clear();
}
