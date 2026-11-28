// ─── Classic presenter ───
// The DOM and particle presentation the overlay has always owned: floating
// numbers, reveals, arcs, mills, ripple fans, forge-and-flips, death clones,
// pending death images, shake and the step's timers. The presenter holds no
// React state; it writes through the setters the host hands it, and the
// elements' identity lives in the host's records, not here.

import { audioManager } from "../../audio/AudioManager.ts";
import { applyScreenShake } from "../../components/animation/ScreenShake.tsx";
import type { ParticleCanvasHandle } from "../../components/animation/ParticleCanvas.tsx";
import { currentSnapshot } from "../../hooks/useGameDispatch.ts";
import type { VfxQuality } from "../types.ts";
import type {
  ActiveCastArc,
  ActiveFloat,
  ActiveMeldForge,
  ActiveMillReveal,
  ActiveReveal,
  ActiveRippleReveal,
  ClassicPresenter,
  DeathClone,
  MeldForgeRequest,
  PendingDeath,
  PendingDeathRequest,
  Point,
  ScreenShakeIntensity,
} from "./types.ts";

let floatIdCounter = 0;
let revealIdCounter = 0;
let shatterIdCounter = 0;
let deathCloneIdCounter = 0;
let castArcIdCounter = 0;
let millRevealIdCounter = 0;
let rippleRevealIdCounter = 0;
let meldForgeIdCounter = 0;

/** The next id for a death effect: a struck card's clone and a ready image's
 *  shatter share one sequence, as they did before the presenters split. */
export function nextDeathCloneId(): number {
  return ++deathCloneIdCounter;
}

/**
 * Resolve the rendered card element for an object id. Collapsed identical-
 * permanent groups (GroupedPermanent collapsed mode) render only their
 * representative card, which carries `data-grouped-ids` listing every id it
 * stands in for — so a non-rendered swarm member falls back to that
 * representative instead of resolving to nothing (no slam, no impact FX).
 */
function findCardElement(objectId: number): HTMLElement | null {
  return (
    document.querySelector<HTMLElement>(`[data-object-id="${objectId}"]`) ??
    document.querySelector<HTMLElement>(`[data-grouped-ids~="${objectId}"]`)
  );
}

/** A stable structural reference; the host owns the actual ref. */
export interface ElementRef<T> {
  readonly current: T | null;
}

export interface ClassicPresenterDeps {
  containerRef: ElementRef<HTMLDivElement>;
  particleRef: ElementRef<ParticleCanvasHandle>;
  pace: number;
  vfxQuality: VfxQuality;
  getPosition: (objectId: number) => DOMRect | undefined;
  setFloats: (update: (previous: ActiveFloat[]) => ActiveFloat[]) => void;
  setDeathClones: (update: (previous: DeathClone[]) => DeathClone[]) => void;
  setReveals: (update: (previous: ActiveReveal[]) => ActiveReveal[]) => void;
  setCastArcs: (update: (previous: ActiveCastArc[]) => ActiveCastArc[]) => void;
  setMillReveals: (update: (previous: ActiveMillReveal[]) => ActiveMillReveal[]) => void;
  setRippleReveals: (update: (previous: ActiveRippleReveal[]) => ActiveRippleReveal[]) => void;
  setMeldForges: (update: (previous: ActiveMeldForge[]) => ActiveMeldForge[]) => void;
  setVignette: (vignette: { damageAmount: number } | null) => void;
  addPendingDeath: (pending: PendingDeath) => void;
  scheduleStepTimeout: (callback: () => void, delay: number) => void;
}

export function createClassicPresenter(deps: ClassicPresenterDeps): ClassicPresenter {
  const objectRect = (objectId: number): DOMRect | null =>
    currentSnapshot.get(objectId) ??
    deps.getPosition(objectId) ??
    findCardElement(objectId)?.getBoundingClientRect() ??
    null;

  const objectPosition = (objectId: number): Point | null => {
    const rect = objectRect(objectId);
    return rect ? { x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 } : null;
  };

  /** Query the actual DOM position of a player's HUD element. */
  const playerHudPosition = (playerId: number): Point => {
    const el = document.querySelector(`[data-player-hud="${playerId}"]`);
    if (el) {
      const rect = el.getBoundingClientRect();
      return { x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 };
    }
    // Fallback: center of screen
    return { x: window.innerWidth / 2, y: window.innerHeight / 2 };
  };

  const screenShake = (intensity: ScreenShakeIntensity): void => {
    const element = deps.containerRef.current;
    if (element) applyScreenShake(element, intensity, deps.pace);
  };

  const particles: ClassicPresenter["particles"] = {
    explosion: (x, y, color) => deps.particleRef.current?.explosion(x, y, color),
    spellImpact: (x, y, color) => deps.particleRef.current?.spellImpact(x, y, color),
    playerDamage: (x, y, amount) => deps.particleRef.current?.playerDamage(x, y, amount),
    healEffect: (x, y, amount) => deps.particleRef.current?.healEffect(x, y, amount),
    summonBurst: (x, y, color) => deps.particleRef.current?.summonBurst(x, y, color),
    slamImpact: (x, y, amount) => deps.particleRef.current?.slamImpact(x, y, amount),
    forgeStrike: (x, y, strength) => deps.particleRef.current?.forgeStrike(x, y, strength),
    forgeHeat: (x, y, durationMs) => deps.particleRef.current?.forgeHeat(x, y, durationMs),
    damageFlurry: (fromPoints, to, hitCount, totalDamage, durationMs) =>
      deps.particleRef.current?.damageFlurry(
        fromPoints,
        to,
        hitCount,
        totalDamage,
        durationMs,
      ),
  };

  return {
    spawnFloat: (position, value, color) => {
      const id = ++floatIdCounter;
      deps.setFloats((prev) => [...prev, { id, value, position, color }]);
    },

    spawnDeathClone: (position, cardName) => {
      const id = nextDeathCloneId();
      deps.setDeathClones((previous) => [...previous, { id, position, cardName }]);
    },

    spawnReveal: (position, colors) => {
      const id = ++revealIdCounter;
      deps.setReveals((prev) => [...prev, { id, position, colors }]);
    },

    spawnCastArc: (from, to, snapshot, mode, timeScale) => {
      const id = ++castArcIdCounter;
      deps.setCastArcs((previous) => [
        ...previous,
        { id, from, to, snapshot, mode, timeScale },
      ]);
    },

    spawnMillReveal: (cards, startIndex, from, to, timeScale) => {
      const id = ++millRevealIdCounter;
      deps.setMillReveals((prev) => [
        ...prev,
        { id, cards, startIndex, from, to, timeScale },
      ]);
    },

    spawnRippleReveal: (cards, from, timeScale) => {
      const id = ++rippleRevealIdCounter;
      deps.setRippleReveals((prev) => [...prev, { id, cards, from, timeScale }]);
    },

    spawnMeldForge: (forge: MeldForgeRequest) => {
      const id = ++meldForgeIdCounter;
      deps.setMeldForges((previous) => [...previous, { id, ...forge }]);
    },

    queuePendingDeath: (request: PendingDeathRequest) => {
      deps.addPendingDeath({
        id: ++shatterIdCounter,
        generation: request.generation,
        snapshot: request.snapshot,
        position: request.position,
        readinessDeadlineMs: request.readinessDeadlineMs,
        timeScale: request.timeScale,
      });
    },

    showVignette: (damageAmount) => deps.setVignette({ damageAmount }),
    clearVignette: () => deps.setVignette(null),

    /** A damage hit landing: its sound, number and shake, and for a player the
     *  vignette. The presenter adds its own particles. */
    landDamageHit: (position, amount, isPlayerTarget) => {
      audioManager.playSfx("DamageDealt");
      const id = ++floatIdCounter;
      deps.setFloats((prev) => [
        ...prev,
        { id, value: -amount, position, color: "#ef4444" },
      ]);

      if (deps.vfxQuality === "full" && deps.containerRef.current) {
        const intensity = amount >= 7 ? "heavy" : amount >= 4 ? "medium" : "light";
        applyScreenShake(deps.containerRef.current, intensity, deps.pace);
      }

      if (isPlayerTarget) {
        deps.setVignette({ damageAmount: amount });
        setTimeout(() => deps.setVignette(null), 500 * deps.pace);
      }
    },

    particles,
    screenShake,

    objectRect,
    snapshotRect: (objectId) => currentSnapshot.get(objectId),
    registryRect: (objectId) => deps.getPosition(objectId),
    objectPosition,
    playerHudPosition,
    damageTargetPosition: (target) =>
      "Player" in target
        ? playerHudPosition(target.Player)
        : (objectPosition(target.Object) ?? {
            x: window.innerWidth / 2,
            y: window.innerHeight / 2,
          }),
    center: (rect) => ({ x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 }),
    cardElement: findCardElement,
    pileRect: (pile, seat) =>
      document.querySelector<HTMLElement>(`[data-${pile}-pile="${seat}"]`)?.getBoundingClientRect() ??
      null,

    scheduleStepTimeout: deps.scheduleStepTimeout,
  };
}
