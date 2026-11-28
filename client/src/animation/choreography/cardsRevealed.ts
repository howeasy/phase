import type { MillCard } from "../../components/animation/MillRevealAnimation.tsx";
import { revealFanCards } from "../../components/animation/revealFanCards.ts";
import { useAnimationStore } from "../../stores/animationStore.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { resolvePileSeat } from "../../viewmodel/gameStateView.ts";
import type { Choreography } from "../presenters/types.ts";

export const presentCardsRevealed: Choreography = (effect, context) => {
  const { event } = effect;
  if (event.type !== "CardsRevealed") return;
  const { classic, pace, vfxQuality } = context;

  // CR 702.60a + CR 701.20b: Ripple (and other "reveal the top N")
  // effects publish their pile without moving it. Fan the revealed
  // cards out of the revealing player's library for every seat to read.
  const { player, card_ids: cardIds = [], card_names: cardNames } = event.data;
  if (vfxQuality === "minimal" || cardNames.length === 0) return;
  const newState = useAnimationStore.getState().animationNewState;
  const revealCards: MillCard[] = revealFanCards(cardIds, cardNames, newState?.objects);
  const libRect = classic.pileRect(
    "library",
    resolvePileSeat(useGameStore.getState().gameState, "library", player),
  );
  const fromPos = libRect
    ? classic.center(libRect)
    : classic.playerHudPosition(player);
  classic.spawnRippleReveal(revealCards, fromPos, Math.min(1, pace));
};
