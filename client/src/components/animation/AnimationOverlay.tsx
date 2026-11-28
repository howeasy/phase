import { AnimatePresence, motion, useReducedMotion } from "framer-motion";
import { type RefObject, useCallback, useEffect, useMemo, useRef, useState } from "react";

// The Director owns the one step clock that advances animationStore's active
// step; the overlay only reports when it has presented a step.
import { director } from "../../animation/director.ts";
import { presentEffect } from "../../animation/choreography/index.ts";
import { createClassicPresenter, nextDeathCloneId } from "../../animation/presenters/classic.ts";
import { createCardVfxPresenter } from "../../animation/presenters/cardVfx.ts";
import type {
  ActiveCastArc,
  ActiveFloat,
  ActiveMeldForge,
  ActiveMillReveal,
  ActiveReveal,
  ActiveRippleReveal,
  ActiveShatter,
  ChoreographyContext,
  DeathClone,
  PendingDeath,
} from "../../animation/presenters/types.ts";
import type { StepEffect } from "../../animation/types.ts";
import { usePresentationPace } from "../../hooks/usePresentationPace.ts";
import { useAnimationStore } from "../../stores/animationStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { CardRevealBurst } from "./CardRevealBurst.tsx";
import { CastArcAnimation } from "./CastArcAnimation.tsx";
import { DamageVignette } from "./DamageVignette.tsx";
import { DeathShatter } from "./DeathShatter.tsx";
import { FloatingNumber } from "./FloatingNumber.tsx";
import { MeldForgeAnimation } from "./MeldForgeAnimation.tsx";
import { MillRevealAnimation } from "./MillRevealAnimation.tsx";
import { RippleRevealAnimation } from "./RippleRevealAnimation.tsx";
import { ParticleCanvas } from "./ParticleCanvas.tsx";
import type { ParticleCanvasHandle } from "./ParticleCanvas.tsx";
import { ResolvedAnimationImage } from "./ResolvedAnimationImage.tsx";
import { CardVfxLayer, type CardVfxLayerHandle, cardVfxSupported } from "./cardVfx/CardVfxLayer.tsx";

type DeathReadinessOutcome =
  | { type: "ready"; image: HTMLImageElement }
  | { type: "fallback" };

interface AnimationOverlayProps {
  containerRef: RefObject<HTMLDivElement | null>;
}

const NO_STEP_EFFECTS: readonly StepEffect[] = [];

function PendingDeathImage({
  pending,
  onSettled,
}: {
  pending: PendingDeath;
  onSettled: (id: number, generation: number, outcome: DeathReadinessOutcome) => void;
}) {
  const settledRef = useRef(false);
  const settle = useCallback((outcome: DeathReadinessOutcome) => {
    if (settledRef.current) return;
    settledRef.current = true;
    onSettled(pending.id, pending.generation, outcome);
  }, [onSettled, pending.generation, pending.id]);

  useEffect(() => {
    const deadline = setTimeout(
      () => settle({ type: "fallback" }),
      pending.readinessDeadlineMs,
    );
    return () => clearTimeout(deadline);
  }, [pending.readinessDeadlineMs, settle]);

  return (
    <div hidden aria-hidden="true">
      <ResolvedAnimationImage
        snapshot={pending.snapshot}
        size="art_crop"
        alt=""
        fallback={null}
        crossOrigin="anonymous"
        onReady={(image) => settle({ type: "ready", image })}
        onExhausted={() => settle({ type: "fallback" })}
      />
    </div>
  );
}

export function AnimationOverlay({ containerRef }: AnimationOverlayProps) {
  const activeStep = useAnimationStore((s) => s.activeStep);
  const activeGeneration = useAnimationStore((s) => s.activeGeneration);
  const getPosition = useAnimationStore((s) => s.getPosition);
  const particleRef = useRef<ParticleCanvasHandle>(null);
  const cardVfxRef = useRef<CardVfxLayerHandle>(null);
  const stepTimeoutsRef = useRef<ReturnType<typeof setTimeout>[]>([]);
  const [activeFloats, setActiveFloats] = useState<ActiveFloat[]>([]);
  const [activeDeathClones, setActiveDeathClones] = useState<DeathClone[]>([]);
  const [activeVignette, setActiveVignette] = useState<{
    damageAmount: number;
  } | null>(null);
  const [activeReveals, setActiveReveals] = useState<ActiveReveal[]>([]);
  const [activeShatters, setActiveShatters] = useState<ActiveShatter[]>([]);
  const [pendingDeaths, setPendingDeaths] = useState<PendingDeath[]>([]);
  const pendingDeathsRef = useRef<PendingDeath[]>([]);
  const [activeCastArcs, setActiveCastArcs] = useState<ActiveCastArc[]>([]);
  const [activeMillReveals, setActiveMillReveals] = useState<ActiveMillReveal[]>([]);
  const [activeRippleReveals, setActiveRippleReveals] = useState<ActiveRippleReveal[]>([]);
  const [activeMeldForges, setActiveMeldForges] = useState<ActiveMeldForge[]>([]);

  const vfxQuality = usePreferencesStore((s) => s.vfxQuality);
  // One clock, one pace: the Director's speed × stack pressure while its run
  // plays, else the speed preference. The overlay schedules no step timer.
  const speedMultiplier = usePresentationPace();
  const reduceMotion = useReducedMotion();
  const cardAnimationStyle = usePreferencesStore((s) => s.cardAnimationStyle);
  // Style Classic, tier minimal, reduced motion and no WebGL 2 mount no layer,
  // so every effect takes the Classic path and no three.js is loaded.
  const cardVfxMounted =
    cardAnimationStyle === "webgl" &&
    vfxQuality !== "minimal" &&
    !reduceMotion &&
    cardVfxSupported();

  const scheduleStepTimeout = useCallback((callback: () => void, delay: number) => {
    const timeout = setTimeout(() => {
      stepTimeoutsRef.current = stepTimeoutsRef.current.filter((id) => id !== timeout);
      callback();
    }, delay);
    stepTimeoutsRef.current.push(timeout);
  }, []);

  const addPendingDeath = useCallback((pending: PendingDeath) => {
    if (useAnimationStore.getState().activeGeneration !== pending.generation) return;
    pendingDeathsRef.current = [...pendingDeathsRef.current, pending];
    setPendingDeaths(pendingDeathsRef.current);
  }, []);

  const settlePendingDeath = useCallback((
    id: number,
    generation: number,
    outcome: DeathReadinessOutcome,
  ) => {
    if (useAnimationStore.getState().activeGeneration !== generation) return;
    const pending = pendingDeathsRef.current.find(
      (record) => record.id === id && record.generation === generation,
    );
    if (!pending) return;

    const remaining = pendingDeathsRef.current.filter((record) => record !== pending);
    if (useAnimationStore.getState().activeGeneration !== generation) return;
    pendingDeathsRef.current = remaining;
    setPendingDeaths(remaining);

    if (useAnimationStore.getState().activeGeneration !== generation) return;
    if (outcome.type === "ready") {
      setActiveShatters((previous) => [
        ...previous,
        {
          id: nextDeathCloneId(),
          position: pending.position,
          image: outcome.image,
          timeScale: pending.timeScale,
        },
      ]);
    } else {
      setActiveDeathClones((previous) => [
        ...previous,
        {
          id: pending.id,
          position: pending.position,
          cardName: pending.snapshot.cardName,
        },
      ]);
    }
  }, []);

  useEffect(() => {
    if (useAnimationStore.getState().activeGeneration !== activeGeneration) return;
    const current = pendingDeathsRef.current.filter(
      (pending) => pending.generation === activeGeneration,
    );
    if (current.length === pendingDeathsRef.current.length) return;
    pendingDeathsRef.current = current;
    setPendingDeaths(current);
  }, [activeGeneration]);

  const classicPresenter = useMemo(
    () =>
      createClassicPresenter({
        containerRef,
        particleRef,
        pace: speedMultiplier,
        vfxQuality,
        getPosition,
        setFloats: setActiveFloats,
        setDeathClones: setActiveDeathClones,
        setReveals: setActiveReveals,
        setCastArcs: setActiveCastArcs,
        setMillReveals: setActiveMillReveals,
        setRippleReveals: setActiveRippleReveals,
        setMeldForges: setActiveMeldForges,
        setVignette: setActiveVignette,
        addPendingDeath,
        scheduleStepTimeout,
      }),
    [
      containerRef,
      speedMultiplier,
      vfxQuality,
      getPosition,
      addPendingDeath,
      scheduleStepTimeout,
    ],
  );

  const cardVfxPresenter = useMemo(
    () => createCardVfxPresenter(() => cardVfxRef.current),
    [],
  );

  const context = useMemo<ChoreographyContext>(
    () => ({
      stepEffects: activeStep?.effects ?? NO_STEP_EFFECTS,
      owningStepMs: (activeStep?.duration ?? 0) * speedMultiplier,
      snapshotSeq: activeStep?.snapshotSeq ?? 0,
      pace: speedMultiplier,
      vfxQuality,
      reduceMotion,
      classic: classicPresenter,
      cardVfx: cardVfxPresenter,
    }),
    [
      activeStep,
      speedMultiplier,
      vfxQuality,
      reduceMotion,
      classicPresenter,
      cardVfxPresenter,
    ],
  );

  useEffect(() => director.attachPresenter(), []);

  // Present the active step's effects, then start the Director's clock for it.
  // The cleanup only drops this step's own sub-step timers (impacts, flashes)
  // so they cannot outlive it.
  useEffect(() => {
    if (!activeStep) return;

    for (const effect of activeStep.effects) {
      presentEffect(effect, context);
    }
    director.presented(activeStep);

    return () => {
      for (const stepTimeout of stepTimeoutsRef.current) clearTimeout(stepTimeout);
      stepTimeoutsRef.current = [];
      classicPresenter.clearVignette();
    };
  }, [activeStep, context, classicPresenter]);

  const handleFloatComplete = useCallback((id: number) => {
    setActiveFloats((prev) => prev.filter((f) => f.id !== id));
  }, []);

  const handleDeathCloneComplete = useCallback((id: number) => {
    setActiveDeathClones((prev) => prev.filter((c) => c.id !== id));
  }, []);

  const handleRevealComplete = useCallback((id: number) => {
    setActiveReveals((prev) => prev.filter((r) => r.id !== id));
  }, []);

  const handleShatterComplete = useCallback((id: number) => {
    setActiveShatters((prev) => prev.filter((s) => s.id !== id));
  }, []);

  const handleCastArcComplete = useCallback((id: number) => {
    setActiveCastArcs((prev) => prev.filter((a) => a.id !== id));
  }, []);

  const handleMillRevealComplete = useCallback((id: number) => {
    setActiveMillReveals((prev) => prev.filter((m) => m.id !== id));
  }, []);

  const handleRippleRevealComplete = useCallback((id: number) => {
    setActiveRippleReveals((prev) => prev.filter((m) => m.id !== id));
  }, []);

  const handleMeldForgeComplete = useCallback((id: number) => {
    setActiveMeldForges((prev) => prev.filter((m) => m.id !== id));
  }, []);

  return (
    <>
      {/* Death clones overlay (z-45) */}
      <div
        style={{
          position: "fixed",
          inset: 0,
          pointerEvents: "none",
          zIndex: 45,
        }}
      >
        <AnimatePresence>
          {activeDeathClones.map((clone) => (
            <motion.div
              key={`death-${clone.id}`}
              initial={{ opacity: 1, scale: 1 }}
              exit={{ opacity: 0, scale: 0.8 }}
              animate={{ opacity: 1, scale: 1 }}
              transition={{ duration: 0.4 * speedMultiplier }}
              onAnimationComplete={() => {
                // Remove after exit animation duration
                setTimeout(
                  () => handleDeathCloneComplete(clone.id),
                  400 * speedMultiplier,
                );
              }}
              style={{
                position: "absolute",
                left: clone.position.x,
                top: clone.position.y,
                width: clone.position.width,
                height: clone.position.height,
                display: "flex",
                alignItems: "center",
                justifyContent: "center",
                fontSize: "0.75rem",
                color: "white",
                backgroundColor: "rgba(0,0,0,0.6)",
                borderRadius: "0.375rem",
                border: "1px solid rgba(239,68,68,0.4)",
              }}
            >
              {clone.cardName}
            </motion.div>
          ))}
        </AnimatePresence>
      </div>

      {/* Hidden, bounded image readiness for creature shatter effects. */}
      {pendingDeaths.map((pending) => (
        <PendingDeathImage
          key={`pending-death-${pending.id}`}
          pending={pending}
          onSettled={settlePendingDeath}
        />
      ))}

      {/* Death shatter effects (z-46) */}
      {activeShatters.map((shatter) => (
        <DeathShatter
          key={`shatter-${shatter.id}`}
          position={shatter.position}
          image={shatter.image}
          timeScale={shatter.timeScale}
          onComplete={() => handleShatterComplete(shatter.id)}
        />
      ))}

      {/* Cast arc animations (z-45) */}
      {activeCastArcs.map((arc) => (
        <CastArcAnimation
          key={`arc-${arc.id}`}
          from={arc.from}
          to={arc.to}
          snapshot={arc.snapshot}
          mode={arc.mode}
          timeScale={arc.timeScale}
          onComplete={() => handleCastArcComplete(arc.id)}
        />
      ))}

      {/* Card flights (z-45), the New style's shared WebGL overlay */}
      {cardVfxMounted && <CardVfxLayer ref={cardVfxRef} tier={vfxQuality} />}

      {/* Mill reveal animations (z-45) */}
      {activeMillReveals.map((mill) => (
        <MillRevealAnimation
          key={`mill-${mill.id}`}
          cards={mill.cards}
          startIndex={mill.startIndex}
          timeScale={mill.timeScale}
          from={mill.from}
          to={mill.to}
          onComplete={() => handleMillRevealComplete(mill.id)}
        />
      ))}

      {/* Ripple reveal animations (z-46) */}
      {activeRippleReveals.map((ripple) => (
        <RippleRevealAnimation
          key={`ripple-${ripple.id}`}
          cards={ripple.cards}
          from={ripple.from}
          timeScale={ripple.timeScale}
          onComplete={() => handleRippleRevealComplete(ripple.id)}
        />
      ))}

      {/* Meld forge-and-flip animations (z-50, beneath the particle sparks) */}
      {activeMeldForges.map((forge) => (
        <MeldForgeAnimation
          key={`meld-${forge.id}`}
          center={forge.center}
          cardSize={forge.cardSize}
          source={forge.source}
          partner={forge.partner}
          result={forge.result}
          durationMs={forge.durationMs}
          onComplete={() => handleMeldForgeComplete(forge.id)}
        />
      ))}

      {/* Damage vignette (z-45) */}
      <DamageVignette
        active={activeVignette != null}
        damageAmount={activeVignette?.damageAmount ?? 0}
        speedMultiplier={speedMultiplier}
      />

      {/* Card reveals */}
      <AnimatePresence>
        {activeReveals.map((reveal) => (
          <CardRevealBurst
            key={`reveal-${reveal.id}`}
            position={reveal.position}
            colors={reveal.colors}
            speedMultiplier={speedMultiplier}
            onComplete={() => handleRevealComplete(reveal.id)}
          />
        ))}
      </AnimatePresence>

      {/* Particles (z-55) */}
      <ParticleCanvas ref={particleRef} />

      {/* Floating numbers (z-60) */}
      <AnimatePresence>
        {activeFloats.map((f) => (
          <FloatingNumber
            key={f.id}
            value={f.value}
            position={f.position}
            color={f.color}
            onComplete={() => handleFloatComplete(f.id)}
            speedMultiplier={speedMultiplier}
          />
        ))}
      </AnimatePresence>
    </>
  );
}
