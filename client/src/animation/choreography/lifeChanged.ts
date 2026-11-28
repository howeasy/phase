import { damageCauseState } from "../../components/animation/cardVfx/cardVfxSpecs.ts";
import { useAnimationStore } from "../../stores/animationStore.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import {
  impactDelayMsForAnimationEvent,
  isPlayerDamageAnimationEvent,
  lifeChangeImpactDelayMs,
} from "../types.ts";
import type { Choreography } from "../presenters/types.ts";

export const presentLifeChanged: Choreography = (effect, context) => {
  const { event } = effect;
  if (event.type !== "LifeChanged") return;
  const { classic, cardVfx, pace, stepEffects, vfxQuality } = context;
  const { player_id, amount, new_total } = event.data;

  // Tick every life readout the moment this hit lands, rather than
  // leaving them on the pre-action snapshot until the whole step queue
  // has drained. The engine supplies the resulting total, so nothing is
  // derived here; an event from a peer that predates the field has none,
  // and those readouts keep their snapshot value as before.
  if (new_total !== undefined) {
    const impactEpoch = useGameStore.getState().engineCommitEpoch;
    classic.scheduleStepTimeout(
      () => useAnimationStore.getState().recordDisplayedLife(
        player_id,
        new_total,
        impactEpoch,
      ),
      lifeChangeImpactDelayMs(effect, stepEffects, player_id, damageCauseState()) * pace,
    );
  }

  // Skip floating number when DamageDealt already covers this player
  // in the same step (avoids duplicate floating numbers)
  const hasDamageDealt = stepEffects.some(
    (e) => isPlayerDamageAnimationEvent(e.event, player_id),
  );
  const groupedDamageEvent = effect.displayOnly
    ? stepEffects.find((e) => e.event.type === "GroupedDamageFlurry")
    : undefined;
  const showLifeChange = () => {
    const { x, y } = classic.playerHudPosition(player_id);
    if (!hasDamageDealt) {
      classic.spawnFloat({ x, y }, amount, amount > 0 ? "#22c55e" : "#ef4444");
    }

    const heal = () => {
      if (amount > 0 && vfxQuality !== "minimal") classic.particles.healEffect(x, y, amount);
    };
    // Under the New style the layer shows the change, unless damage
    // this step already shows the loss it causes (CR 120.3a: damage
    // to a player makes them lose that much life). A gain is never
    // damage's, so it always shows.
    const lossShownByDamage = amount < 0 && (hasDamageDealt || groupedDamageEvent !== undefined);
    if (cardVfx.available && !lossShownByDamage) {
      cardVfx.present({ kind: "life", playerId: player_id, amount, pace }, heal);
    } else {
      heal();
    }
  };

  if (groupedDamageEvent) {
    classic.scheduleStepTimeout(
      showLifeChange,
      impactDelayMsForAnimationEvent(groupedDamageEvent.event) * pace,
    );
  } else {
    showLifeChange();
  }
};
