import { hexToRgb } from "../../components/animation/particleEffects.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getCardColors } from "../wubrgColors.ts";
import type { Choreography } from "../presenters/types.ts";

export const presentTokenCreated: Choreography = (effect, context) => {
  const { event } = effect;
  if (event.type !== "TokenCreated") return;
  const { classic, vfxQuality } = context;

  const { object_id } = event.data;
  const pos = classic.objectPosition(object_id);
  if (pos) {
    const gameState = useGameStore.getState().gameState;
    const colors = gameState?.objects[object_id]?.color ?? [];
    classic.spawnReveal(pos, getCardColors(colors));

    if (vfxQuality !== "minimal") {
      const tokenColor = colors.length > 0 ? hexToRgb(getCardColors(colors)[0]) : undefined;
      classic.particles.summonBurst(pos.x, pos.y, tokenColor);
    }
  }
};
