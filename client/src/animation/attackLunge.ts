/** Display-only attack-lunge properties, banded by the attacker's power. */
export interface AttackLungeBand {
  /** Highest power in this band, inclusive. The last band is open-ended. */
  maxPower: number | null;
  /** Full lunge duration at speed multiplier 1, in milliseconds. */
  durationMs: number;
  /** Wind-up distance as a fraction of the slide magnitude. */
  windUp: number;
  /** Thrust distance past the resting slide, as a fraction of its magnitude. */
  thrust: number;
  /** Peak scale at the thrust. */
  peakScale: number;
  /** Fraction of the duration spent holding the wind-up; 0 has no hang. */
  hang: number;
}

/**
 * The single numeric authority for the committed-attacker lunge. Tests and
 * later animation phases read these values instead of restating them.
 * A `null` power (or a missing one) reads as the 2–3 band.
 */
export const ATTACK_LUNGE_BANDS: readonly AttackLungeBand[] = [
  { maxPower: 1, durationMs: 360, windUp: 0.25, thrust: 1.6, peakScale: 1.04, hang: 0 },
  { maxPower: 3, durationMs: 460, windUp: 0.4, thrust: 1.9, peakScale: 1.08, hang: 0 },
  { maxPower: 7, durationMs: 640, windUp: 0.55, thrust: 2.15, peakScale: 1.11, hang: 0.25 },
  { maxPower: null, durationMs: 780, windUp: 0.65, thrust: 2.35, peakScale: 1.14, hang: 0.3 },
];

/** The band for `power`; `null`/missing reads as the 2–3 band. */
export function attackLungeBand(power: number | null | undefined): AttackLungeBand {
  const value = power ?? 2;
  // The last band is open-ended, so this always finds a match.
  return (
    ATTACK_LUNGE_BANDS.find(
      (band) => band.maxPower === null || value <= band.maxPower,
    ) ?? ATTACK_LUNGE_BANDS[ATTACK_LUNGE_BANDS.length - 1]
  );
}
