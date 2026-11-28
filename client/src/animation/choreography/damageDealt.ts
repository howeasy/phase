import { audioManager } from "../../audio/AudioManager.ts";
import {
  applyCardKnockback,
  applyCardSlam,
} from "../../components/animation/CardSlamAnimation.tsx";
import { CARD_SLAM_FLIGHT_MS } from "../types.ts";
import type { Choreography } from "../presenters/types.ts";
import { visiblePreEventSnapshot } from "./snapshots.ts";

export const presentDamageDealt: Choreography = (effect, context) => {
  const { event } = effect;
  if (event.type !== "DamageDealt") return;
  const { classic, cardVfx, owningStepMs, pace, stepEffects, vfxQuality } = context;

  const { source_id, target, amount } = event.data;
  const pos = classic.damageTargetPosition(target);

  // Creature-on-creature: slam the actual card element (Arena-style)
  if ("Object" in target && vfxQuality !== "minimal") {
    // For bidirectional pairs (combat or fight), only slam the first direction.
    // The second direction still gets its floating damage number below.
    const effectIndex = stepEffects.indexOf(effect);
    const isPairedReturn = stepEffects.slice(0, effectIndex).some((e) => {
      if (e.event.type !== "DamageDealt") return false;
      const other = e.event.data.target;
      if (!("Object" in other)) return false;
      return e.event.data.source_id === target.Object && other.Object === source_id;
    });

    // Resolve via the group representative when this attacker is a
    // non-rendered member of a collapsed swarm (findCardElement).
    const sourceEl = isPairedReturn ? null : classic.cardElement(source_id);
    const sourceAt = sourceEl && classic.center(sourceEl.getBoundingClientRect());
    // Under the New style the struck card rocks back from the blow.
    const struckEl = cardVfx.available ? classic.cardElement(target.Object) : null;
    let landBlow = () => classic.particles.slamImpact(pos.x, pos.y, amount);
    let knockBack = () => {
      if (struckEl && sourceAt) {
        applyCardKnockback(
          struckEl,
          pos.x - sourceAt.x,
          pos.y - sourceAt.y,
          amount,
          pace,
        );
      }
    };
    // One start for the slam and its blow, so both land on one frame.
    const slamStartMs = performance.now();
    const slammed = sourceEl
      ? applyCardSlam(
          sourceEl,
          pos.x,
          pos.y,
          pace,
          () => {
            // Impact effects: SFX, shockwave, floating number, screen shake
            landBlow();
            knockBack();
            classic.landDamageHit(pos, amount, false);
          },
          slamStartMs,
          effect.strikeBand,
        )
      : false;
    if (slammed) {
      const impactDelayMs = (effect.strikeBand?.flightMs ?? CARD_SLAM_FLIGHT_MS) * pace;
      const slam = { amount, pace, startMs: slamStartMs, impactDelayMs };
      landBlow = cardVfx.presentBlow(
        { kind: "blow", sourceId: source_id, target, ...slam },
        landBlow,
      );
      if (struckEl) {
        knockBack = cardVfx.presentBlow(
          {
            kind: "knockback",
            objectId: target.Object,
            face: visiblePreEventSnapshot(target.Object),
            sourceId: source_id,
            owningStepMs,
            ...slam,
          },
          knockBack,
        );
      }
    }

    if (!slammed) {
      // Paired return, missing source element, or representative already
      // mid-slam: show the floating damage number (+ SFX) without a slam.
      const showHit = () => {
        audioManager.playSfx("DamageDealt");
        classic.spawnFloat(pos, -amount, "#ef4444");
      };
      // A combat pair is one exchange: its return lands when the slam does.
      if (isPairedReturn && effect.strikeBand) {
        classic.scheduleStepTimeout(showHit, effect.strikeBand.flightMs * pace);
      } else {
        showHit();
      }
    }
    return;
  }

  // Creature-to-player: card slam at the player HUD
  {
    // Resolve via the group representative for a collapsed swarm member.
    const sourceEl = vfxQuality !== "minimal" ? classic.cardElement(source_id) : null;
    let landBlow = () => classic.particles.playerDamage(pos.x, pos.y, amount);
    const slamStartMs = performance.now();
    const slammed = sourceEl
      ? applyCardSlam(
          sourceEl,
          pos.x,
          pos.y,
          pace,
          () => {
            landBlow();
            classic.landDamageHit(pos, amount, "Player" in target);
          },
          slamStartMs,
          effect.strikeBand,
        )
      : false;
    if (slammed) {
      landBlow = cardVfx.presentBlow(
        {
          kind: "blow",
          sourceId: source_id,
          target,
          amount,
          pace,
          startMs: slamStartMs,
          impactDelayMs: (effect.strikeBand?.flightMs ?? CARD_SLAM_FLIGHT_MS) * pace,
        },
        landBlow,
      );
    }

    if (!slammed) {
      audioManager.playSfx("DamageDealt");
      classic.spawnFloat(pos, -amount, "#ef4444");
    }
  }
};
