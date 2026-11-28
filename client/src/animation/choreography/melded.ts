import { audioManager } from "../../audio/AudioManager.ts";
import type { MeldForgePiece } from "../../components/animation/MeldForgeAnimation.tsx";
import { FORGE_YELLOW } from "../../components/animation/particleEffects.ts";
import { useAnimationStore } from "../../stores/animationStore.ts";
import { MELD_FORGE_PHASES } from "../types.ts";
import type { Choreography } from "../presenters/types.ts";
import { visiblePostEventSnapshot, visiblePreEventSnapshot } from "./snapshots.ts";

/** Card size used when neither meld card has a measured board position. */
const MELD_FALLBACK_CARD_SIZE = { width: 80, height: 112 };

export const presentMelded: Choreography = (effect, context) => {
  const { event } = effect;
  if (event.type !== "Melded") return;
  const { classic, owningStepMs, reduceMotion, vfxQuality } = context;

  // CR 701.42a: forge the pair on the board, then turn the fused blank
  // over to the melded permanent's combined face.
  if (owningStepMs <= 0) return;
  const { object_id, partner_id } = event.data;
  const sourceRect = classic.objectRect(object_id);
  const partnerRect = classic.objectRect(partner_id);
  const measured = [sourceRect, partnerRect].filter((rect): rect is DOMRect => rect != null);
  const centers = measured.map((rect) => classic.center(rect));
  const center = centers.length > 0
    ? {
        x: centers.reduce((sum, point) => sum + point.x, 0) / centers.length,
        y: centers.reduce((sum, point) => sum + point.y, 0) / centers.length,
      }
    : { x: window.innerWidth / 2, y: window.innerHeight / 2 };
  // A tapped card's rect is landscape; the forge works in portrait.
  const cardSize = measured.length > 0
    ? {
        width: Math.min(measured[0].width, measured[0].height),
        height: Math.max(measured[0].width, measured[0].height),
      }
    : MELD_FALLBACK_CARD_SIZE;
  const piece = (objectId: number, rect: DOMRect | null, side: -1 | 1): MeldForgePiece => {
    const at = rect ? classic.center(rect) : null;
    return {
      snapshot: visiblePreEventSnapshot(objectId),
      offset: at
        ? { x: at.x - center.x, y: at.y - center.y }
        : { x: side * cardSize.width * 1.5, y: 0 },
    };
  };
  // The forge lifts both cards off the board, so their board cards hide
  // for the rest of this step.
  useAnimationStore.getState().veilObjects([object_id, partner_id]);
  classic.spawnMeldForge({
    center,
    cardSize,
    source: piece(object_id, sourceRect, -1),
    partner: piece(partner_id, partnerRect, 1),
    result: visiblePostEventSnapshot(object_id),
    durationMs: owningStepMs,
  });

  // The hammer blows always ring. Particles and screen shake move the
  // board, so they honor reduced motion as well as the VFX quality —
  // as the forge itself does by cross-fading instead.
  const animatesVfx = vfxQuality !== "minimal" && !reduceMotion;
  const at = (fraction: number) => fraction * owningStepMs;
  if (animatesVfx) {
    classic.scheduleStepTimeout(
      () => classic.particles.forgeHeat(
        center.x,
        center.y,
        at(MELD_FORGE_PHASES.fused - MELD_FORGE_PHASES.gathered),
      ),
      at(MELD_FORGE_PHASES.gathered),
    );
  }
  MELD_FORGE_PHASES.strikes.forEach((strikeAt, blow) => {
    const isFinalBlow = blow === MELD_FORGE_PHASES.strikes.length - 1;
    classic.scheduleStepTimeout(() => {
      audioManager.playSfx("DamageDealt", isFinalBlow ? 1 : 0.7);
      if (!animatesVfx) return;
      classic.particles.forgeStrike(
        center.x,
        center.y,
        (blow + 1) / MELD_FORGE_PHASES.strikes.length,
      );
      if (vfxQuality === "full") {
        classic.screenShake(isFinalBlow ? "medium" : "light");
      }
    }, at(strikeAt));
  });
  if (animatesVfx) {
    classic.scheduleStepTimeout(
      () => classic.particles.summonBurst(center.x, center.y, FORGE_YELLOW),
      at(MELD_FORGE_PHASES.flipped),
    );
  }
};
