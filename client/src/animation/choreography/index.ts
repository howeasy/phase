// ─── Choreography registry ───
// One module per event kind, dispatched by event type. The host never switches
// on an event itself; it calls `presentEffect`, which offers the event to the
// card VFX layer first and otherwise runs the event's Classic choreography.

import { cardVfxSpecFor } from "../../components/animation/cardVfx/cardVfxSpecs.ts";
import { useAnimationStore } from "../../stores/animationStore.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import type { AnimationEvent, StepEffect } from "../types.ts";
import type { Choreography, ChoreographyContext } from "../presenters/types.ts";
import { presentCardsRevealed } from "./cardsRevealed.ts";
import { presentDamageDealt } from "./damageDealt.ts";
import { presentDestruction } from "./destruction.ts";
import { presentGroupedDamageFlurry } from "./groupedDamageFlurry.ts";
import { presentLifeChanged } from "./lifeChanged.ts";
import { presentMelded } from "./melded.ts";
import { presentSpellCast } from "./spellCast.ts";
import { presentTokenCreated } from "./tokenCreated.ts";
import { presentTurnStarted } from "./turnStarted.ts";
import { presentZoneChanged } from "./zoneChanged.ts";

const CHOREOGRAPHIES: Partial<Record<AnimationEvent["type"], Choreography>> = {
  GroupedDamageFlurry: presentGroupedDamageFlurry,
  DamageDealt: presentDamageDealt,
  LifeChanged: presentLifeChanged,
  CreatureDestroyed: presentDestruction,
  PermanentSacrificed: presentDestruction,
  SpellCast: presentSpellCast,
  TurnStarted: presentTurnStarted,
  ZoneChanged: presentZoneChanged,
  Melded: presentMelded,
  TokenCreated: presentTokenCreated,
  CardsRevealed: presentCardsRevealed,
};

function presentClassicEffect(effect: StepEffect, context: ChoreographyContext): void {
  CHOREOGRAPHIES[effect.event.type]?.(effect, context);
}

/** Present one step effect: the card VFX layer's spec, or its Classic
 *  choreography. The layer runs the Classic effect itself when it declines, so
 *  each effect is shown exactly once. */
export function presentEffect(effect: StepEffect, context: ChoreographyContext): void {
  const { event } = effect;
  const classic = () => presentClassicEffect(effect, context);
  const spec = context.cardVfx.available
    ? cardVfxSpecFor(event, {
        pre: useGameStore.getState().gameState,
        post: useAnimationStore.getState().animationNewState,
        pace: context.pace,
        owningStepMs: context.owningStepMs,
        snapshotSeq: context.snapshotSeq,
        stepEvents: context.stepEffects.map((stepEffect) => stepEffect.event),
      })
    : null;
  const onImpact =
    event.type === "DamageDealt"
      ? () => context.classic.landDamageHit(
          context.classic.damageTargetPosition(event.data.target),
          event.data.amount,
          "Player" in event.data.target,
        )
      : undefined;
  if (spec) context.cardVfx.present(spec, classic, onImpact);
  else classic();
}
