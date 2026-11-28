import { motion, useReducedMotion } from "framer-motion";
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";

import type { ObjectId, StackEntry, StackEntryDisplay } from "../../adapter/types.ts";
import { GAME_Z_LAYER } from "../../constants/ui.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { stackEntryDescription } from "../../utils/stackEntryDescription.ts";
import { RichLabel } from "../mana/RichLabel.tsx";

/** Ability tags shown for one commit before the rest collapse into a counter. */
const MAX_TAGS = 3;
const TAG_WIDTH_PX = 220;
const TAG_HEIGHT_PX = 40;
const TAG_GAP_PX = 6;
/** Appearance spacing between tags created by one commit. */
const TAG_STAGGER_MS = 150;
/** Full lifetime at the reference multiplier; the multiplier shortens, never lengthens. */
const TAG_LIFE_MS = 1600;
/** The card's own hop. */
const HOP_MS = 260;

interface TriggerTag {
  key: string;
  entryId: ObjectId;
  /** Card element the tag was measured against. A collapsed group shares one. */
  anchor: HTMLElement;
  /** Slot above or below the anchor, so tags on one card never share a position. */
  depth: number;
  text: string;
  left: number;
  top: number;
  width: number;
  delayMs: number;
  /** Entries a "+N more" summary stands for; empty on an ability's own tag. */
  summarized: ObjectId[];
}

interface TagCandidate {
  entry: StackEntry;
  text: string;
  element: HTMLElement;
}

const EMPTY_TAG_LIST: TriggerTag[] = [];
const EMPTY_DETAILS: Record<string, StackEntryDisplay> = {};

function findSourceCard(sourceId: ObjectId): HTMLElement | null {
  return (
    document.querySelector<HTMLElement>(`[data-object-id="${sourceId}"]`) ??
    document.querySelector<HTMLElement>(`[data-grouped-ids~="${sourceId}"]`)
  );
}

interface Frame {
  left: number;
  top: number;
  width: number;
  height: number;
}

function placement(
  rect: DOMRect,
  stackIndex: number,
  frame: Frame,
): { left: number; top: number; width: number } {
  const width = Math.min(TAG_WIDTH_PX, Math.max(40, frame.width - 8));
  const maxLeft = Math.max(4, frame.width - width - 4);
  // Card coordinates are viewport-relative; the layer is fixed to its own
  // containing block, which `contain-paint` can make narrower than the window.
  const cardLeft = rect.left - frame.left;
  const cardTop = rect.top - frame.top;
  const centerX = cardLeft + rect.width / 2;
  const left = Math.min(Math.max(centerX - width / 2, 4), maxLeft);

  // Below a card in the top half, above one in the bottom half.
  const below = cardTop + rect.height / 2 < frame.height / 2;
  const step = TAG_HEIGHT_PX + TAG_GAP_PX;
  const rawTop = below
    ? cardTop + rect.height + TAG_GAP_PX + stackIndex * step
    : cardTop - TAG_GAP_PX - TAG_HEIGHT_PX - stackIndex * step;
  const top = Math.min(Math.max(rawTop, 4), Math.max(4, frame.height - TAG_HEIGHT_PX - 4));
  return { left, top, width };
}

/** A short hop of the source card, added to (never replacing) its own transform. */
function hopCard(element: HTMLElement, speedScale: number): void {
  if (typeof element.animate !== "function") return;
  element.animate(
    [
      { transform: "translateY(0px)" },
      { transform: "translateY(-4px)" },
      { transform: "translateY(0px)" },
    ],
    { duration: HOP_MS * speedScale, easing: "ease-out", composite: "add" },
  );
}

/**
 * Tags rising from the battlefield cards whose triggered abilities just reached
 * the stack. Driven by committed `events` (the board and stack only render the
 * committed state), never by an animation step, so an entering creature is on
 * the page when its own action is announced.
 */
export function TriggerTagLayer() {
  const { t } = useTranslation("game");
  const events = useGameStore((s) => s.events);
  const stack = useGameStore((s) => s.gameState?.stack);
  const generation = useGameStore((s) => s.gameSessionGeneration);
  const reducedMotion = useReducedMotion() === true;
  const [tags, setTags] = useState<TriggerTag[]>(EMPTY_TAG_LIST);

  const tagsRef = useRef<TriggerTag[]>(EMPTY_TAG_LIST);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const taggedEntryIds = useRef<Set<ObjectId>>(new Set());
  const lastEvents = useRef(events);
  const lastGeneration = useRef(generation);
  const timers = useRef<Set<number>>(new Set());
  const tagKeySeed = useRef(0);

  const publish = useCallback((next: TriggerTag[]) => {
    tagsRef.current = next.length === 0 ? EMPTY_TAG_LIST : next;
    setTags(tagsRef.current);
  }, []);

  const removeTag = useCallback(
    (key: string) => {
      publish(tagsRef.current.filter((tag) => tag.key !== key));
    },
    [publish],
  );

  const scheduleRemoval = useCallback(
    (key: string, afterMs: number) => {
      const timer = window.setTimeout(() => {
        timers.current.delete(timer);
        removeTag(key);
      }, afterMs);
      timers.current.add(timer);
    },
    [removeTag],
  );

  const clearAll = useCallback(() => {
    for (const timer of timers.current) window.clearTimeout(timer);
    timers.current.clear();
    taggedEntryIds.current.clear();
    publish(EMPTY_TAG_LIST);
  }, [publish]);

  // An id is spent only while its entry stands: once it has left the stack, a
  // later push under that id (an undone action taken again) is a new entry. A
  // restore need not report events, so this follows the stack itself.
  useLayoutEffect(() => {
    for (const id of taggedEntryIds.current) {
      if (!stack?.some((entry) => entry.id === id)) taggedEntryIds.current.delete(id);
    }
  }, [stack]);

  useLayoutEffect(() => {
    // A new session owns no tag and no timer from the previous one. Consuming the
    // events present at this boundary keeps a rejoin/resync commit from tagging.
    if (generation !== lastGeneration.current) {
      lastGeneration.current = generation;
      lastEvents.current = events;
      clearAll();
      return;
    }
    // Mount/StrictMode and re-renders replay the same committed array.
    if (events === lastEvents.current) return;
    lastEvents.current = events;
    if (events.length === 0) return;
    if (typeof document === "undefined" || document.visibilityState === "hidden") return;

    const speed = usePreferencesStore.getState().animationSpeedMultiplier;
    if (!(speed > 0)) return;

    const { gameState } = useGameStore.getState();
    if (!gameState) return;

    const stackById = new Map(gameState.stack.map((entry) => [String(entry.id), entry]));
    const detailsByEntry = gameState.derived?.stack_entry_details ?? EMPTY_DETAILS;
    const queued = new Set<ObjectId>();
    const candidates: TagCandidate[] = [];

    for (const event of events) {
      if (event.type !== "StackPushed") continue;
      const entry = stackById.get(String(event.data.object_id));
      if (!entry || entry.kind.type !== "TriggeredAbility") continue;
      if (taggedEntryIds.current.has(entry.id) || queued.has(entry.id)) continue;
      const source = gameState.objects[String(entry.source_id)];
      if (!source || source.zone !== "Battlefield") continue;

      // Only the committed stack entry and its display row carry the words;
      // the source object supplies the same `~` substituent StackEntry uses.
      const details = detailsByEntry[String(entry.id)];
      const sourceName =
        details?.source_name || entry.kind.data.source_name || source.name || "";
      const text = stackEntryDescription(entry, details, sourceName);
      if (!text) continue;

      const element = findSourceCard(entry.source_id);
      if (!element) continue;

      queued.add(entry.id);
      candidates.push({ entry, text, element });
    }

    if (candidates.length === 0) return;

    const summaryText = (ids: ObjectId[]) => t("triggerTag.more", { n: ids.length });
    // Whatever still stands for an id being pushed again is replaced: its own
    // tag goes, and a summary stops counting it.
    const pushedAgain = new Set(candidates.map((candidate) => candidate.entry.id));
    let existing = tagsRef.current.flatMap((tag): TriggerTag[] => {
      if (tag.summarized.length === 0) return pushedAgain.has(tag.entryId) ? [] : [tag];
      const kept = tag.summarized.filter((id) => !pushedAgain.has(id));
      if (kept.length === tag.summarized.length) return [tag];
      return kept.length === 0 ? [] : [{ ...tag, summarized: kept, text: summaryText(kept) }];
    });
    // The cap counts ability tags; one "+N more" summary reports the rest.
    const activeAbilityTags = existing.filter((tag) => tag.summarized.length === 0).length;
    const room = Math.max(0, MAX_TAGS - activeAbilityTags);
    const shown = candidates.slice(0, room);
    const omitted = candidates.slice(shown.length).map((candidate) => candidate.entry.id);
    // A standing summary is folded into the new one, never shown beside it.
    let carried: ObjectId[] = [];
    if (omitted.length > 0) {
      carried = existing.flatMap((tag) => tag.summarized);
      existing = existing.filter((tag) => tag.summarized.length === 0);
    }
    const speedScale = Math.min(speed, 1);
    const lifetimeMs = TAG_LIFE_MS * speedScale;

    const created: TriggerTag[] = [];

    // Positions are relative to the layer's own fixed box. In the game that box
    // is the paint-contained board column; in the gallery it is the window.
    const rootRect = rootRef.current?.getBoundingClientRect();
    const frame: Frame =
      rootRect && rootRect.width > 0 && rootRect.height > 0
        ? { left: rootRect.left, top: rootRect.top, width: rootRect.width, height: rootRect.height }
        : { left: 0, top: 0, width: window.innerWidth, height: window.innerHeight };

    const place = (candidate: TagCandidate, text: string, summarized: ObjectId[]): TriggerTag => {
      const taken = new Set(
        [...existing, ...created]
          .filter((tag) => tag.anchor === candidate.element)
          .map((tag) => tag.depth),
      );
      let depth = 0;
      while (taken.has(depth)) depth += 1;
      const rect = candidate.element.getBoundingClientRect();
      const { left, top, width } = placement(rect, depth, frame);
      return {
        key: `tag-${generation}-${++tagKeySeed.current}`,
        entryId: candidate.entry.id,
        anchor: candidate.element,
        depth,
        text,
        left,
        top,
        width,
        delayMs: 0,
        summarized,
      };
    };

    for (const candidate of shown) {
      const tag = place(candidate, candidate.text, []);
      tag.delayMs = created.length * TAG_STAGGER_MS;
      created.push(tag);
      scheduleRemoval(tag.key, tag.delayMs + lifetimeMs);
    }

    const firstOmitted = candidates[shown.length];
    if (firstOmitted) {
      const total = [...carried, ...omitted];
      const summary = place(firstOmitted, summaryText(total), total);
      summary.delayMs = created.length * TAG_STAGGER_MS;
      created.push(summary);
      scheduleRemoval(summary.key, summary.delayMs + lifetimeMs);
    }

    // Every represented entry is spent: a re-commit cannot tag it again.
    for (const candidate of candidates) taggedEntryIds.current.add(candidate.entry.id);

    if (!reducedMotion) {
      const hopped = new Set<HTMLElement>();
      for (const candidate of [...shown, ...(firstOmitted ? [firstOmitted] : [])]) {
        if (hopped.has(candidate.element)) continue;
        hopped.add(candidate.element);
        hopCard(candidate.element, speedScale);
      }
    }

    publish([...existing, ...created]);
  }, [events, generation, reducedMotion, t, publish, scheduleRemoval, clearAll]);

  useEffect(
    () => () => {
      for (const timer of timers.current) window.clearTimeout(timer);
      timers.current.clear();
    },
    [],
  );

  return (
    <div
      ref={rootRef}
      data-trigger-tag-layer
      aria-hidden
      className={`pointer-events-none fixed inset-0 ${GAME_Z_LAYER.boardChoiceGrid}`}
    >
      {tags.map((tag) => (
        <motion.div
          key={tag.key}
          data-trigger-tag={tag.summarized.length > 0 ? undefined : String(tag.entryId)}
          data-trigger-tag-more={tag.summarized.length > 0 ? "" : undefined}
          className="pointer-events-none absolute line-clamp-2 rounded-md border border-white/20 bg-gray-950/90 px-1.5 py-1 text-center text-[11px] font-medium leading-[14px] text-gray-100 shadow-lg"
          style={{
            left: tag.left,
            top: tag.top,
            width: tag.width,
            maxHeight: TAG_HEIGHT_PX,
            overflow: "hidden",
          }}
          initial={{ opacity: 0, y: reducedMotion ? 0 : 4 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ duration: reducedMotion ? 0.2 : 0.16, delay: tag.delayMs / 1000 }}
        >
          <RichLabel text={tag.text} size="xs" />

        </motion.div>
      ))}
    </div>
  );
}
