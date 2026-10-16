import {
  MotionGlobalConfig,
  animateTarget,
  frameData,
  frameSteps,
  getValueTransition,
  getVariantContext,
  isAnimationControls,
  resolveTransition,
  resolveVariant,
  variantPriorityOrder,
  visualElementStore,
  type AnimationDefinition,
  type AnimationType,
  type StepId,
  type TargetAndTransition,
  type Transition,
  type VisualElement,
} from "framer-motion";

/**
 * Attribute set on `<html>` while the document is hidden. `index.css` keys a
 * rule off it that turns every CSS keyframe animation and transition off.
 */
export const DOCUMENT_HIDDEN_ATTRIBUTE = "data-document-hidden";

/**
 * How often framer-motion's frameloop is driven by hand while hidden. Browsers
 * clamp hidden-tab timers to at least 1 s (and, after several minutes, to one
 * wake-up per minute), so this only bounds how long finished work waits.
 */
export const MOTION_FRAME_PUMP_MS = 250;

let disposeInstalled: (() => void) | null = null;

/**
 * A hidden tab renders no frames: `requestAnimationFrame` never fires, and
 * neither do Web Animations `finish` events, which are dispatched as part of a
 * rendered frame. Anything that waits for a frame waits until the tab is shown
 * again, and a game keeps updating the board in the
 * meantime (AI seats, opponents, remote updates). Two kinds of UI then pile up
 * without bound:
 *
 * - CSS animations and transitions (`animate-pulse`, `transition-*`). When
 *   something forces style resolution while hidden — framer-motion layout
 *   measurement does, every revision — Chromium creates the CSSAnimation /
 *   CSSTransition and holds it until the next frame. Unmounting the element
 *   does not release it, so every waiting-reason pill, permanent-card frame, …
 *   that mounts and unmounts while hidden stays in memory as detached DOM.
 * - framer-motion exits. `AnimatePresence` keeps an exiting child mounted until
 *   its exit animation completes. framer-motion completes animations from its
 *   rAF-driven frameloop, instant ones included, or from a WAAPI `finish`
 *   event. Exiting stack items, hand cards, overlays and badges stay attached.
 *
 * Both are released once the tab is visible again, so hidden-tab memory grows
 * with time spent hidden. While the document is hidden this:
 *
 * - sets {@link DOCUMENT_HIDDEN_ATTRIBUTE} so no CSS animation or transition
 *   is created;
 * - sets framer-motion's global `skipAnimations`, so animations jump to their
 *   final values;
 * - runs framer-motion's frameloop steps on a timer, which completes those
 *   jumps and lets `AnimatePresence` remove exited children.
 *
 * When the tab is shown again everything is restored and rAF resumes driving
 * the frameloop. Skipping finishes an infinite `repeat` loop that started while
 * hidden (an attack pulse, a prompt glow, a hovered tile's sparks) on its last
 * keyframe, so running loops are restarted (see {@link restartMotionLoops}). Returns a disposer; installing
 * twice replaces the earlier installation.
 */
export function installHiddenDocumentMotion(doc: Document = document): () => void {
  disposeInstalled?.();

  const root = doc.documentElement;
  let pump: ReturnType<typeof setInterval> | null = null;
  let skipAnimationsBeforeHide: boolean | undefined;

  const enterHidden = () => {
    if (pump !== null) return;
    root.setAttribute(DOCUMENT_HIDDEN_ATTRIBUTE, "");
    skipAnimationsBeforeHide = MotionGlobalConfig.skipAnimations;
    MotionGlobalConfig.skipAnimations = true;
    flushMotionFrame();
    pump = setInterval(flushMotionFrame, MOTION_FRAME_PUMP_MS);
  };

  const leaveHidden = () => {
    if (pump === null) return;
    clearInterval(pump);
    pump = null;
    MotionGlobalConfig.skipAnimations = skipAnimationsBeforeHide;
    root.removeAttribute(DOCUMENT_HIDDEN_ATTRIBUTE);
    restartMotionLoops(root);
  };

  const sync = () => {
    if (doc.visibilityState === "hidden") enterHidden();
    else leaveHidden();
  };

  doc.addEventListener("visibilitychange", sync);
  sync();

  const dispose = () => {
    doc.removeEventListener("visibilitychange", sync);
    leaveHidden();
    if (disposeInstalled === dispose) disposeInstalled = null;
  };
  disposeInstalled = dispose;
  return dispose;
}

/**
 * framer-motion's frameloop step order. Its `stepsOrder` is declared in the
 * type definitions but not exported at runtime, so it is mirrored here.
 */
export const MOTION_FRAME_STEPS = [
  "setup",
  "read",
  "resolveKeyframes",
  "preUpdate",
  "update",
  "preRender",
  "render",
  "postRender",
] as const satisfies readonly StepId[];

/**
 * Run one framer-motion frame outside `requestAnimationFrame`, step by step in
 * the frameloop's own order, as its rAF batch would.
 */
function flushMotionFrame(): void {
  const timestamp = performance.now();
  frameData.delta = Math.max(timestamp - frameData.timestamp, 0);
  frameData.timestamp = timestamp;
  frameData.isProcessing = true;
  try {
    for (const step of MOTION_FRAME_STEPS) frameSteps[step].process(frameData);
  } finally {
    frameData.isProcessing = false;
  }
}

/**
 * Restart every infinite `repeat` loop under `root` that a motion element is
 * currently running, whether it is declared on its own `animate`, inherited
 * from a variant-controlling parent, or set by an active gesture
 * (`whileHover`, ...). Only the looping values are replayed, so finite
 * animations, which ended on their final values while skipped, and the
 * element's animation state (active gestures, presence) are left as they are.
 * Restarting a loop that kept running only resets its phase.
 */
function restartMotionLoops(root: Element): void {
  for (const element of root.querySelectorAll("*")) {
    const visualElement = visualElementStore.get(element);
    if (!visualElement) continue;
    const loops = activeMotionLoops(visualElement);
    // No animation type: animateTarget would skip values the type's state
    // already considers settled.
    if (loops) animateTarget(visualElement, loops);
  }
}

/**
 * The values of `visualElement` whose active animation repeats forever, with
 * their transitions, resolved the way framer-motion's animation state resolves
 * them: active types from highest priority down, each value owned by the
 * highest-priority type that sets it. `null` when there are none, or while the
 * element is exiting, since replaying would interrupt its exit animation and
 * leave it mounted.
 */
function activeMotionLoops(visualElement: VisualElement): TargetAndTransition | null {
  const typeStates = visualElement.animationState?.getState();
  if (!typeStates || typeStates.exit?.isActive || visualElement.presenceContext?.isPresent === false) {
    return null;
  }
  const props = visualElement.getProps() as Partial<Record<AnimationType, unknown>>;
  const context = (getVariantContext(visualElement.parent) ?? {}) as Partial<
    Record<AnimationType, unknown>
  >;
  const defaultTransition = visualElement.getDefaultTransition();
  const ownedKeys = new Set<string>();
  const loops: Record<string, unknown> = {};
  const loopTransitions: Record<string, Transition> = {};

  for (const type of [...variantPriorityOrder].reverse()) {
    if (type === "exit" || !typeStates[type]?.isActive) continue;
    const definition = props[type] ?? context[type];
    if (!definition || typeof definition === "boolean" || isAnimationControls(definition)) continue;
    // Later variants in a list override earlier ones. Copy before reversing:
    // the list belongs to the component's props.
    const variants = Array.isArray(definition) ? [...definition].reverse() : [definition];
    for (const variant of variants) {
      const resolved = resolveVariant(visualElement, variant as AnimationDefinition);
      if (!resolved) continue;
      const { transition, transitionEnd, ...target } = resolved;
      const effective = transition ? resolveTransition(transition, defaultTransition) : defaultTransition;
      for (const [key, value] of Object.entries(target)) {
        if (ownedKeys.has(key)) continue;
        ownedKeys.add(key);
        const valueTransition = getValueTransition(effective ?? {}, key) as Transition | undefined;
        if (valueTransition?.repeat !== Infinity) continue;
        loops[key] = value;
        loopTransitions[key] = valueTransition;
      }
      // Terminal values count toward ownership too: animation state resolves a
      // type as `{ ...target, ...transitionEnd }` and protects every such key
      // from lower-priority types.
      for (const key of Object.keys(transitionEnd ?? {})) ownedKeys.add(key);
    }
  }
  return Object.keys(loops).length > 0
    ? ({ ...loops, transition: loopTransitions } as TargetAndTransition)
    : null;
}
