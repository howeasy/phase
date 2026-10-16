import { act, cleanup, render } from "@testing-library/react";
import { AnimatePresence, MotionGlobalConfig, frameSteps, motion, visualElementStore } from "framer-motion";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  DOCUMENT_HIDDEN_ATTRIBUTE,
  MOTION_FRAME_PUMP_MS,
  MOTION_FRAME_STEPS,
  installHiddenDocumentMotion,
} from "../hiddenDocumentMotion";

// A hidden tab never runs requestAnimationFrame callbacks. framer-motion binds
// its frameloop to rAF when the module loads, so freeze rAF before the import.
const rafCallbacks = vi.hoisted(() => {
  const queued: FrameRequestCallback[] = [];
  globalThis.requestAnimationFrame = (callback) => {
    queued.push(callback);
    return queued.length;
  };
  return queued;
});

let visibilityState: DocumentVisibilityState = "visible";

function setVisibility(next: DocumentVisibilityState) {
  visibilityState = next;
  document.dispatchEvent(new Event("visibilitychange"));
}

/** Keyed badge that exits and re-enters on every change, like a seat's
 *  waiting-reason pill moving between priority holders. */
function Badge({ revision }: { revision: number }) {
  return (
    <AnimatePresence>
      <motion.span
        key={revision}
        data-testid="badge"
        initial={{ opacity: 0 }}
        animate={{ opacity: 1 }}
        exit={{ opacity: 0 }}
        transition={{ duration: 0.15 }}
      />
    </AnimatePresence>
  );
}

/** Run queued animation frames, the way a visible tab would. */
function runAnimationFrames(count: number) {
  for (let i = 0; i < count; i += 1) {
    vi.advanceTimersByTime(16);
    for (const callback of rafCallbacks.splice(0)) callback(performance.now());
  }
}

describe("installHiddenDocumentMotion", () => {
  let dispose: (() => void) | undefined;

  beforeEach(() => {
    vi.useFakeTimers();
    visibilityState = "visible";
    vi.spyOn(document, "visibilityState", "get").mockImplementation(() => visibilityState);
    vi.spyOn(document, "hidden", "get").mockImplementation(() => visibilityState === "hidden");
  });

  afterEach(() => {
    dispose?.();
    dispose = undefined;
    cleanup();
    // Run, never drop, the pending frame: framer-motion's frameloop waits for
    // it before it requests another one.
    runAnimationFrames(1);
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("drives every framer-motion frameloop step, in frameloop order", () => {
    // frameSteps is built by reducing over framer-motion's step order.
    expect(MOTION_FRAME_STEPS).toEqual(Object.keys(frameSteps));
  });

  it("does not let AnimatePresence exits pile up while the tab is hidden", async () => {
    dispose = installHiddenDocumentMotion(document);
    setVisibility("hidden");

    const { container, rerender } = render(<Badge revision={0} />);
    for (let revision = 1; revision <= 20; revision += 1) {
      rerender(<Badge revision={revision} />);
      await act(() => vi.advanceTimersByTimeAsync(MOTION_FRAME_PUMP_MS));
    }

    // Only the current badge is mounted; every exited one was released.
    expect(container.querySelectorAll('[data-testid="badge"]')).toHaveLength(1);
  });

  it("restarts an infinite loop that mounted while the tab was hidden", async () => {
    dispose = installHiddenDocumentMotion(document);
    setVisibility("hidden");

    const { getByTestId } = render(
      <motion.div
        data-testid="loop"
        animate={{ left: ["0px", "100px", "0px"] }}
        transition={{ duration: 1, repeat: Infinity }}
      />,
    );
    await act(() => vi.advanceTimersByTimeAsync(MOTION_FRAME_PUMP_MS * 4));

    await act(async () => {
      setVisibility("visible");
    });
    const lefts = new Set<string>();
    for (let frame = 0; frame < 30; frame += 1) {
      await act(async () => runAnimationFrames(1));
      lefts.add(getByTestId("loop").style.left);
    }

    expect(lefts.size).toBeGreaterThan(5);
  });

  async function distinctLefts(element: HTMLElement, frames = 30): Promise<number> {
    const lefts = new Set<string>();
    for (let frame = 0; frame < frames; frame += 1) {
      await act(async () => runAnimationFrames(1));
      lefts.add(element.style.left);
    }
    return lefts.size;
  }

  it("restarts a loop inherited from a variant-controlling parent", async () => {
    dispose = installHiddenDocumentMotion(document);
    setVisibility("hidden");

    const { getByTestId } = render(
      <motion.div animate="on">
        <motion.div
          data-testid="child-loop"
          variants={{
            on: { left: ["0px", "100px", "0px"], transition: { duration: 1, repeat: Infinity } },
          }}
        />
      </motion.div>,
    );
    await act(() => vi.advanceTimersByTimeAsync(MOTION_FRAME_PUMP_MS * 4));
    await act(async () => {
      setVisibility("visible");
    });

    expect(await distinctLefts(getByTestId("child-loop"))).toBeGreaterThan(5);
  });

  it("keeps an active gesture's loop running, and its gesture active, across a hide", async () => {
    dispose = installHiddenDocumentMotion(document);
    const { getByTestId } = render(
      <motion.div
        data-testid="hover-loop"
        animate={{ top: ["0px", "50px", "0px"] }}
        transition={{ duration: 1, repeat: Infinity }}
        whileHover={{ left: ["0px", "100px", "0px"], transition: { duration: 1, repeat: Infinity } }}
      />,
    );
    const element = getByTestId("hover-loop");
    const animationState = visualElementStore.get(element)?.animationState;
    // What framer-motion's hover gesture does on pointer enter.
    await act(async () => {
      void animationState?.setActive("whileHover", true);
      runAnimationFrames(2);
    });

    setVisibility("hidden");
    await act(() => vi.advanceTimersByTimeAsync(MOTION_FRAME_PUMP_MS * 4));
    await act(async () => {
      setVisibility("visible");
    });

    expect(await distinctLefts(element)).toBeGreaterThan(5);
    expect(animationState?.getState().whileHover.isActive).toBe(true);
  });

  async function hideAndShow() {
    setVisibility("hidden");
    await act(() => vi.advanceTimersByTimeAsync(MOTION_FRAME_PUMP_MS * 4));
    await act(async () => {
      setVisibility("visible");
    });
  }

  it("leaves a variant list untouched and its later finite variant in charge", async () => {
    dispose = installHiddenDocumentMotion(document);
    const labels = ["loop", "settle"];
    const { getByTestId } = render(
      <motion.div
        data-testid="variant-list"
        animate={labels}
        variants={{
          loop: { left: ["0px", "100px", "0px"], transition: { duration: 1, repeat: Infinity } },
          settle: { left: "200px", transition: { duration: 0 } },
        }}
      />,
    );
    await act(async () => runAnimationFrames(2));

    for (let cycle = 0; cycle < 2; cycle += 1) {
      await hideAndShow();
      expect(labels).toEqual(["loop", "settle"]);
      expect(await distinctLefts(getByTestId("variant-list"), 10)).toBe(1);
    }
  });

  it("does not replay a loop on a value a higher-priority gesture settles", async () => {
    dispose = installHiddenDocumentMotion(document);
    setVisibility("hidden");
    const { getByTestId } = render(
      <motion.div
        data-testid="settled-by-hover"
        animate={{ left: ["0px", "100px", "0px"] }}
        transition={{ duration: 1, repeat: Infinity }}
        whileHover={{ top: "10px", transitionEnd: { left: "200px" } }}
      />,
    );
    const element = getByTestId("settled-by-hover");
    await act(async () => {
      void visualElementStore.get(element)?.animationState?.setActive("whileHover", true);
      await vi.advanceTimersByTimeAsync(MOTION_FRAME_PUMP_MS * 4);
    });

    await act(async () => {
      setVisibility("visible");
    });

    expect(await distinctLefts(element, 20)).toBe(1);
  });

  it("lets an exit that was running when the tab hid finish after it is shown", async () => {
    dispose = installHiddenDocumentMotion(document);
    function Looping({ shown }: { shown: boolean }) {
      return (
        <AnimatePresence>
          {shown && (
            <motion.div
              key="looping"
              data-testid="exiting"
              animate={{ left: ["0px", "100px", "0px"] }}
              exit={{ left: "200px", transition: { duration: 0.5 } }}
              transition={{ duration: 1, repeat: Infinity }}
            />
          )}
        </AnimatePresence>
      );
    }
    const { queryByTestId, rerender } = render(<Looping shown />);
    await act(async () => runAnimationFrames(2));

    rerender(<Looping shown={false} />);
    await act(async () => runAnimationFrames(2));
    setVisibility("hidden");
    await act(async () => {
      setVisibility("visible");
    });
    for (let frame = 0; frame < 60 && queryByTestId("exiting"); frame += 1) {
      await act(async () => runAnimationFrames(1));
    }

    expect(queryByTestId("exiting")).toBeNull();
  });

  it("gates CSS motion and framer-motion only while the document is hidden", () => {
    MotionGlobalConfig.skipAnimations = false;
    dispose = installHiddenDocumentMotion(document);
    const root = document.documentElement;

    expect(root.hasAttribute(DOCUMENT_HIDDEN_ATTRIBUTE)).toBe(false);
    expect(MotionGlobalConfig.skipAnimations).toBe(false);

    setVisibility("hidden");
    expect(root.hasAttribute(DOCUMENT_HIDDEN_ATTRIBUTE)).toBe(true);
    expect(MotionGlobalConfig.skipAnimations).toBe(true);

    setVisibility("visible");
    expect(root.hasAttribute(DOCUMENT_HIDDEN_ATTRIBUTE)).toBe(false);
    expect(MotionGlobalConfig.skipAnimations).toBe(false);
  });

  it("restores the document and motion config when disposed while hidden", () => {
    MotionGlobalConfig.skipAnimations = false;
    dispose = installHiddenDocumentMotion(document);
    setVisibility("hidden");

    dispose();
    dispose = undefined;

    expect(document.documentElement.hasAttribute(DOCUMENT_HIDDEN_ATTRIBUTE)).toBe(false);
    expect(MotionGlobalConfig.skipAnimations).toBe(false);
    expect(vi.getTimerCount()).toBe(0);
  });
});
