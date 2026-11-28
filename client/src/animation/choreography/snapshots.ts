import type { AnimationImageSnapshot } from "../../components/animation/ResolvedAnimationImage.tsx";
import { visibleAnimationImageSnapshot } from "../../components/animation/ResolvedAnimationImage.tsx";
import { useAnimationStore } from "../../stores/animationStore.ts";
import { useGameStore } from "../../stores/gameStore.ts";

export function visiblePreEventSnapshot(objectId: number): AnimationImageSnapshot | null {
  return visibleAnimationImageSnapshot(
    useGameStore.getState().gameState?.objects[objectId],
  );
}

export function visiblePostEventSnapshot(objectId: number): AnimationImageSnapshot | null {
  return visibleAnimationImageSnapshot(
    useAnimationStore.getState().animationNewState?.objects[objectId],
  );
}
