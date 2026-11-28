import { castAnnounced } from "../../components/animation/cardVfx/cardFlightSpecs.ts";
import { hexToRgb } from "../../components/animation/particleEffects.ts";
import { useAnimationStore } from "../../stores/animationStore.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getCardColors } from "../wubrgColors.ts";
import type { Choreography } from "../presenters/types.ts";
import { visiblePostEventSnapshot } from "./snapshots.ts";

export const presentSpellCast: Choreography = (effect, context) => {
  const { event } = effect;
  if (event.type !== "SpellCast") return;
  const { classic, pace, vfxQuality } = context;

  const { object_id } = event.data;
  const pos = classic.objectPosition(object_id);
  if (pos) {
    const newObject = useAnimationStore.getState().animationNewState?.objects[object_id];
    const snapshot = visiblePostEventSnapshot(object_id);
    const colors = snapshot ? newObject?.color ?? [] : [];
    const burstColor = getCardColors(colors)[0] ?? "#06b6d4";
    // Under the New style a spell announced in an earlier batch
    // already flew to the stack; only the burst marks its cast.
    const flown =
      useAnimationStore.getState().cardVfxReady &&
      castAnnounced(object_id, useGameStore.getState().gameState);
    if (vfxQuality !== "minimal") {
      classic.particles.spellImpact(pos.x, pos.y, hexToRgb(burstColor));
      if (!flown) {
        const stackPos = { x: window.innerWidth * 0.75, y: window.innerHeight * 0.4 };
        classic.spawnCastArc(pos, stackPos, snapshot, "cast", Math.min(1, pace));
      }
    }
  }
};
