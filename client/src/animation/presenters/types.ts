// ─── Choreography presenters ───
// What an event choreography is allowed to ask for. A choreography receives one
// of these and does nothing else: every element, particle, position, timer and
// card VFX offer goes through here, so the same body runs above the Classic
// DOM presentation and above the card VFX layer.

import type { TargetRef } from "../../adapter/types.ts";
import type { MeldForgePiece } from "../../components/animation/MeldForgeAnimation.tsx";
import type { MillCard } from "../../components/animation/MillRevealAnimation.tsx";
import type { RGB } from "../../components/animation/particleSystem.ts";
import type { AnimationImageSnapshot } from "../../components/animation/ResolvedAnimationImage.tsx";
import type {
  CardVfxSpec,
  DamageBlowSpec,
  DamageKnockbackSpec,
} from "../../components/animation/cardVfx/cardVfxSpecs.ts";
import type { StepEffect, VfxQuality } from "../types.ts";

export interface Point {
  x: number;
  y: number;
}

export interface CardRect extends Point {
  width: number;
  height: number;
}

export interface Size {
  width: number;
  height: number;
}

export interface ActiveFloat {
  id: number;
  value: number;
  position: Point;
  color: string;
}

export interface DeathClone {
  id: number;
  position: CardRect;
  cardName: string | null;
}

export interface ActiveReveal {
  id: number;
  position: Point;
  colors: string[];
}

export interface ActiveShatter {
  id: number;
  position: CardRect;
  image: HTMLImageElement;
  /** Fixed when the death was presented, so a later pace change cannot retime it. */
  timeScale: number;
}

export type CastArcMode = "cast" | "resolve-permanent" | "resolve-spell";

export interface ActiveCastArc {
  id: number;
  from: Point;
  to: Point;
  snapshot: AnimationImageSnapshot | null;
  mode: CastArcMode;
  /** Fixed when the arc spawns, so a later pace change cannot retime it mid-flight. */
  timeScale: number;
}

export interface ActiveMillReveal {
  id: number;
  cards: MillCard[];
  startIndex: number;
  from: Point;
  to: Point;
  timeScale: number;
}

export interface ActiveRippleReveal {
  id: number;
  cards: MillCard[];
  from: Point;
  /** Fixed when the reveal spawns, so a later pace change cannot retime it. */
  timeScale: number;
}

export interface ActiveMeldForge {
  id: number;
  center: Point;
  cardSize: Size;
  source: MeldForgePiece;
  partner: MeldForgePiece;
  result: AnimationImageSnapshot | null;
  durationMs: number;
}

export interface PendingDeath {
  id: number;
  generation: number;
  snapshot: AnimationImageSnapshot;
  position: CardRect;
  readinessDeadlineMs: number;
  /** Captured when the death is presented, not when its image resolves: a
   *  later pace change must not retime the shatter this death becomes. */
  timeScale: number;
}

/** What a choreography asks the Classic presenter to queue, before it is
 *  stamped with the pending image's id. */
export interface PendingDeathRequest {
  generation: number;
  snapshot: AnimationImageSnapshot;
  position: CardRect;
  readinessDeadlineMs: number;
  timeScale: number;
}

/** One forge-and-flip, as the Classic presenter stores it. */
export interface MeldForgeRequest {
  center: Point;
  cardSize: Size;
  source: MeldForgePiece;
  partner: MeldForgePiece;
  result: AnimationImageSnapshot | null;
  durationMs: number;
}

export type ScreenShakeIntensity = "light" | "medium" | "heavy";

/** The particle canvas, narrowed to what the choreographies call. */
export interface ParticleEffectsPresenter {
  explosion(x: number, y: number, color?: RGB): void;
  spellImpact(x: number, y: number, color?: RGB): void;
  playerDamage(x: number, y: number, amount: number): void;
  healEffect(x: number, y: number, amount: number): void;
  summonBurst(x: number, y: number, color?: RGB): void;
  slamImpact(x: number, y: number, amount: number): void;
  forgeStrike(x: number, y: number, strength: number): void;
  forgeHeat(x: number, y: number, durationMs: number): void;
  damageFlurry(
    fromPoints: Point[],
    to: Point,
    hitCount: number,
    totalDamage: number,
    durationMs: number,
  ): void;
}

/** The Classic presentation: DOM elements, framer-style sprites, the particle
 *  canvas, screen shake, positions and the step's own timers. */
export interface ClassicPresenter {
  spawnFloat(position: Point, value: number, color: string): void;
  spawnDeathClone(position: CardRect, cardName: string | null): void;
  spawnReveal(position: Point, colors: string[]): void;
  spawnCastArc(
    from: Point,
    to: Point,
    snapshot: AnimationImageSnapshot | null,
    mode: CastArcMode,
    timeScale: number,
  ): void;
  spawnMillReveal(
    cards: MillCard[],
    startIndex: number,
    from: Point,
    to: Point,
    timeScale: number,
  ): void;
  spawnRippleReveal(cards: MillCard[], from: Point, timeScale: number): void;
  spawnMeldForge(forge: MeldForgeRequest): void;
  queuePendingDeath(request: PendingDeathRequest): void;

  showVignette(damageAmount: number): void;
  clearVignette(): void;
  /** A damage hit landing: its sound, number and shake, and for a player the
   *  vignette. The presenter adds its own particles. */
  landDamageHit(position: Point, amount: number, isPlayerTarget: boolean): void;

  readonly particles: ParticleEffectsPresenter;
  screenShake(intensity: ScreenShakeIntensity): void;

  objectRect(objectId: number): DOMRect | null;
  snapshotRect(objectId: number): DOMRect | undefined;
  registryRect(objectId: number): DOMRect | undefined;
  objectPosition(objectId: number): Point | null;
  /** Query the actual DOM position of a player's HUD element. */
  playerHudPosition(playerId: number): Point;
  /** Where a damage event's target is on screen. */
  damageTargetPosition(target: TargetRef): Point;
  center(rect: DOMRect): Point;
  cardElement(objectId: number): HTMLElement | null;
  pileRect(pile: "library" | "graveyard", seat: number): DOMRect | null;

  scheduleStepTimeout(callback: () => void, delay: number): void;
}

/** The card VFX layer, read through a ref at presentation time. `present` runs
 *  `classic` itself when a mounted layer declines; it never runs both. */
export interface CardVfxPresenter {
  readonly available: boolean;
  present(spec: CardVfxSpec, classic: () => void, onImpact?: () => void): void;
  /** Present a blow or knockback and return what lands at its impact — the
   *  Classic effect if the layer could not present it. */
  presentBlow(
    spec: DamageBlowSpec | DamageKnockbackSpec,
    classic: () => void,
  ): () => void;
}

export interface ChoreographyContext {
  readonly stepEffects: readonly StepEffect[];
  readonly owningStepMs: number;
  readonly snapshotSeq: number;
  readonly pace: number;
  readonly vfxQuality: VfxQuality;
  /** `null` while the reduced-motion query has no answer yet. */
  readonly reduceMotion: boolean | null;
  readonly classic: ClassicPresenter;
  readonly cardVfx: CardVfxPresenter;
}

/** One event kind's Classic presentation. The registry only calls it for its
 *  own event type; each module narrows `effect.event` before reading the
 *  payload. */
export type Choreography = (effect: StepEffect, context: ChoreographyContext) => void;
