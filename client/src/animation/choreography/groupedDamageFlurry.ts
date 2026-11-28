import { audioManager } from "../../audio/AudioManager.ts";
import {
  DAMAGE_FLURRY_SOURCE_SAMPLE_LIMIT,
  impactDelayMsForAnimationEvent,
} from "../types.ts";
import type { Choreography, Point } from "../presenters/types.ts";

export const presentGroupedDamageFlurry: Choreography = (effect, context) => {
  const { event } = effect;
  if (event.type !== "GroupedDamageFlurry") return;
  const { classic, cardVfx, pace, vfxQuality } = context;

  const { player_id, source_ids, total_damage, hit_count } = event.data;
  const to = classic.playerHudPosition(player_id);
  const fromPoints = source_ids
    .slice(0, DAMAGE_FLURRY_SOURCE_SAMPLE_LIMIT)
    .map((sourceId) => classic.objectPosition(sourceId))
    .filter((position): position is Point => position != null);
  const origins = fromPoints.length > 0 ? fromPoints : [to];
  const impactDelay = impactDelayMsForAnimationEvent(event) * pace;
  let landBlow = () => classic.particles.playerDamage(to.x, to.y, total_damage);

  if (vfxQuality !== "minimal") {
    classic.particles.damageFlurry(origins, to, hit_count, total_damage, impactDelay);
    landBlow = cardVfx.presentBlow(
      {
        kind: "blow",
        sourceId: null,
        target: { Player: player_id },
        amount: total_damage,
        pace,
        startMs: performance.now(),
        impactDelayMs: impactDelay,
      },
      landBlow,
    );
  }

  classic.scheduleStepTimeout(() => {
    audioManager.playSfx("DamageDealt");
    classic.spawnFloat(to, -total_damage, "#ef4444");

    if (vfxQuality !== "minimal") {
      landBlow();
      classic.showVignette(total_damage);
      classic.scheduleStepTimeout(() => classic.clearVignette(), 500 * pace);
    }

    if (vfxQuality === "full") {
      const intensity = total_damage >= 20 ? "heavy" : total_damage >= 10 ? "medium" : "light";
      classic.screenShake(intensity);
    }
  }, impactDelay);
};
