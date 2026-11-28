import type { MillCard } from "../../components/animation/MillRevealAnimation.tsx";
import { hexToRgb } from "../../components/animation/particleEffects.ts";
import { useAnimationStore } from "../../stores/animationStore.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { resolvePileSeat } from "../../viewmodel/gameStateView.ts";
import { getCardColors } from "../wubrgColors.ts";
import type { Choreography } from "../presenters/types.ts";
import { visiblePostEventSnapshot, visiblePreEventSnapshot } from "./snapshots.ts";

export const presentZoneChanged: Choreography = (effect, context) => {
  const { event } = effect;
  if (event.type !== "ZoneChanged") return;
  const { classic, pace, stepEffects, vfxQuality } = context;

  const { object_id, from: fromZone, to: toZone } = event.data;
  if (toZone === "Battlefield") {
    const pos = classic.objectPosition(object_id);
    if (pos) {
      const gameState = useGameStore.getState().gameState;
      const colors = gameState?.objects[object_id]?.color ?? [];
      classic.spawnReveal(pos, getCardColors(colors));

      if (vfxQuality !== "minimal") {
        const summonColor = colors.length > 0 ? hexToRgb(getCardColors(colors)[0]) : undefined;
        classic.particles.summonBurst(pos.x, pos.y, summonColor);

        if (fromZone === "Stack") {
          const snapshot = visiblePreEventSnapshot(object_id);
          const stackPos = { x: window.innerWidth * 0.75, y: window.innerHeight * 0.4 };
          classic.spawnCastArc(stackPos, pos, snapshot, "resolve-permanent", Math.min(1, pace));
        }
      }
    }
  } else if (fromZone === "Stack" && toZone === "Graveyard") {
    if (vfxQuality !== "minimal") {
      const snapshot = visiblePreEventSnapshot(object_id);
      const stackPos = { x: window.innerWidth * 0.75, y: window.innerHeight * 0.4 };
      classic.spawnCastArc(stackPos, stackPos, snapshot, "resolve-spell", Math.min(1, pace));
    }
  } else if (fromZone === "Library" && toZone === "Graveyard") {
    // CR 701.17a: each milled card goes from its owner's library to
    // their graveyard. Every event presents its own card, leaving in
    // its place in the step's order, so a card whose flight presents
    // it is never shown twice and one that falls back is never dropped.
    if (vfxQuality !== "minimal") {
      const object = useAnimationStore.getState().animationNewState?.objects[object_id];
      const snapshot = visiblePostEventSnapshot(object_id);
      const card: MillCard = {
        objectId: object_id,
        snapshot,
        colors: snapshot ? getCardColors(object?.color ?? []) : [],
      };
      const startIndex = stepEffects
        .filter(
          ({ event: other }) =>
            other.type === "ZoneChanged" &&
            other.data.from === "Library" &&
            other.data.to === "Graveyard",
        )
        .indexOf(effect);
      const ownerId = object?.owner ?? 0;
      const pileState = useGameStore.getState().gameState;
      const libSeat = resolvePileSeat(pileState, "library", ownerId);
      const gySeat = resolvePileSeat(pileState, "graveyard", ownerId);
      const libRect = classic.pileRect("library", libSeat);
      const gyRect = classic.pileRect("graveyard", gySeat);
      const hudFallback = classic.playerHudPosition(ownerId);
      classic.spawnMillReveal(
        [card],
        Math.max(startIndex, 0),
        libRect ? classic.center(libRect) : hudFallback,
        gyRect ? classic.center(gyRect) : hudFallback,
        Math.min(1, pace),
      );
    }
  }
};
