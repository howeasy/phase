import { hexToRgb } from "../../components/animation/particleEffects.ts";
import { useAnimationStore } from "../../stores/animationStore.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getCardColors } from "../wubrgColors.ts";
import type { Choreography } from "../presenters/types.ts";
import { visiblePreEventSnapshot } from "./snapshots.ts";

/** Between a death's presentation and its shatter, its face may still load. */
const DEATH_IMAGE_READY_MAX_MS = 250;

export const presentDestruction: Choreography = (effect, context) => {
  const { event } = effect;
  if (event.type !== "CreatureDestroyed" && event.type !== "PermanentSacrificed") return;
  const { classic, owningStepMs, pace, vfxQuality } = context;

  const { object_id } = event.data;
  const imageSnapshot = visiblePreEventSnapshot(object_id);
  const pos = classic.objectPosition(object_id);
  if (pos && vfxQuality !== "minimal") {
    const colors = imageSnapshot
      ? useGameStore.getState().gameState?.objects[object_id]?.color ?? []
      : [];
    const explosionColor = colors.length > 0 ? hexToRgb(getCardColors(colors)[0]) : undefined;
    classic.particles.explosion(pos.x, pos.y, explosionColor);
  }

  const snapshotRect = classic.snapshotRect(object_id);
  const registryRect = classic.registryRect(object_id);
  const rect = snapshotRect ?? registryRect;
  if (rect) {
    const position = { x: rect.x, y: rect.y, width: rect.width, height: rect.height };
    if (
      vfxQuality !== "minimal" &&
      event.type === "CreatureDestroyed" &&
      imageSnapshot
    ) {
      const generation = useAnimationStore.getState().activeGeneration;
      classic.queuePendingDeath({
        generation,
        snapshot: imageSnapshot,
        position,
        readinessDeadlineMs: Math.min(
          DEATH_IMAGE_READY_MAX_MS,
          owningStepMs * 0.75,
        ),
        timeScale: Math.min(1, pace),
      });
    } else {
      classic.spawnDeathClone(position, imageSnapshot?.cardName ?? null);
    }
  }
};
