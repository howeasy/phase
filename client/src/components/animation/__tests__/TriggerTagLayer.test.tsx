import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameEvent, GameObject, GameState, ObjectId } from "../../../adapter/types.ts";
import { useGameStore } from "../../../stores/gameStore.ts";
import { usePreferencesStore } from "../../../stores/preferencesStore.ts";
import { StackEntry } from "../../stack/StackEntry.tsx";
import { buildObjectMap, gameObjectFactory } from "../../../test/factories/gameObjectFactory.ts";
import {
  buildGameState,
  buildStackEntry,
} from "../../../test/factories/gameStateFactory.ts";
import { TriggerTagLayer } from "../TriggerTagLayer.tsx";

vi.mock("../../../hooks/useCardImage.ts", () => ({
  useCardImage: () => ({ src: "/test-card.png", isLoading: false }),
}));
vi.mock("../../../game/dispatch.ts", () => ({ dispatchAction: vi.fn() }));

const TRIGGER = 80;
// Elvish Visionary's printed text, the whole of it.
const NAME = "Elvish Visionary";
const TEXT = "When this creature enters, draw a card.";

function push(objectId: ObjectId): GameEvent {
  return { type: "StackPushed", data: { object_id: objectId } };
}

function source(id: ObjectId = 7): GameObject {
  return gameObjectFactory
    .creature(1, 1)
    .onBattlefield()
    .colored("Green")
    .withId(id)
    .named(NAME)
    .params({ card_types: { subtypes: ["Elf", "Shaman"] } })
    .build();
}

function triggerEntry(id = TRIGGER, sourceId = 7) {
  return buildStackEntry({
    id,
    source_id: sourceId,
    controller: 0,
    kind: {
      type: "TriggeredAbility",
      data: {
        source_id: sourceId,
        source_name: NAME,
        description: TEXT,
        ability: { targets: [] },
      },
    },
  });
}

function stateWith(
  entry = triggerEntry(),
  object = source(),
  extraObjects: GameObject[] = [],
): GameState {
  return buildGameState({
    objects: buildObjectMap(object, ...extraObjects),
    battlefield: object.zone === "Battlefield" ? [object.id, ...extraObjects.map((o) => o.id)] : [],
    stack: [entry],
    derived: {
      stack_entry_details: {
        [String(entry.id)]: {
          source_name: NAME,
          kind_label: "Triggered",
          ability_kind: { type: "Triggered" },
        },
      },
    },
  });
}

function rect(left: number, top: number, width = 80, height = 112): DOMRect {
  return {
    x: left,
    y: top,
    left,
    top,
    width,
    height,
    right: left + width,
    bottom: top + height,
    toJSON: () => ({}),
  } as DOMRect;
}

function renderLayer(ids: ObjectId[] = [7]) {
  return render(
    <>
      {ids.map((id) => (
        <div key={id} data-object-id={id} />
      ))}
      <TriggerTagLayer />
    </>,
  );
}

function measure(id: ObjectId, left = 400, top = 100) {
  const element = document.querySelector<HTMLElement>(`[data-object-id="${id}"]`);
  if (!element) throw new Error(`missing anchor ${id}`);
  element.getBoundingClientRect = () => rect(left, top);
}

function commit(state: GameState, events: GameEvent[]) {
  act(() => {
    useGameStore.setState({ gameState: state, events });
  });
}

beforeEach(() => {
  useGameStore.getState().reset();
  usePreferencesStore.setState({ animationSpeedMultiplier: 1 });
});

afterEach(() => {
  cleanup();
  useGameStore.getState().reset();
  Reflect.deleteProperty(document, "visibilityState");
  vi.restoreAllMocks();
});

describe("TriggerTagLayer", () => {
  it("tags an entering creature's triggered ability with the stack's own words", () => {
    const entry = triggerEntry();
    const state = stateWith(entry);
    renderLayer();
    measure(7);

    commit(state, [push(entry.id)]);

    expect(document.querySelector('[data-trigger-tag="80"]')?.textContent).toBe(TEXT);
  });

  it("shows the same description StackEntry shows", () => {
    const entry = triggerEntry();
    const stack = render(
      <StackEntry
        entry={entry}
        details={{
          source_name: NAME,
          kind_label: "Triggered",
          ability_kind: { type: "Triggered" },
        }}
        index={0}
        isTop
        cardSize={{ width: 120, height: 168 }}
      />,
    );
    expect(screen.getAllByText(TEXT).length).toBeGreaterThan(0);
    stack.unmount();

    const state = stateWith(entry);
    renderLayer();
    measure(7);
    commit(state, [push(entry.id)]);

    expect(document.querySelector('[data-trigger-tag="80"]')?.textContent).toBe(TEXT);
  });

  it("tags nothing for a spell pushed onto the stack", () => {
    const spell = buildStackEntry({
      id: 81,
      source_id: 7,
      controller: 0,
      kind: { type: "Spell", data: { card_id: 70, ability: { targets: [] } } },
    });
    const state = stateWith(spell);
    renderLayer();
    measure(7);

    commit(state, [push(spell.id)]);

    expect(document.querySelector("[data-trigger-tag]")).toBeNull();
  });

  it("tags nothing when the source is not on the battlefield", () => {
    const entry = triggerEntry();
    const inHand = gameObjectFactory.creature(1, 1).inHand().withId(7).named(NAME).build();
    const state = stateWith(entry, inHand);
    renderLayer();
    measure(7);

    commit(state, [push(entry.id)]);

    expect(document.querySelector("[data-trigger-tag]")).toBeNull();
  });

  it("tags nothing for an entry no longer on the committed stack", () => {
    const state = buildGameState({
      objects: buildObjectMap(source()),
      battlefield: [7],
      stack: [],
    });
    renderLayer();
    measure(7);

    commit(state, [push(TRIGGER)]);

    expect(document.querySelector("[data-trigger-tag]")).toBeNull();
  });

  it("tags nothing for events already committed when it mounts", () => {
    const entry = triggerEntry();
    const state = stateWith(entry);
    act(() => {
      useGameStore.setState({ gameState: state, events: [push(entry.id)] });
    });
    renderLayer();
    measure(7);

    expect(document.querySelector("[data-trigger-tag]")).toBeNull();
  });

  it("tags nothing for an empty-events commit", () => {
    const entry = triggerEntry();
    const state = stateWith(entry);
    renderLayer();
    measure(7);

    commit(state, []);

    expect(document.querySelector("[data-trigger-tag]")).toBeNull();
  });

  it("tags nothing at speed 0", () => {
    usePreferencesStore.setState({ animationSpeedMultiplier: 0 });
    const entry = triggerEntry();
    const state = stateWith(entry);
    renderLayer();
    measure(7);

    commit(state, [push(entry.id)]);

    expect(document.querySelector("[data-trigger-tag]")).toBeNull();
  });

  it("tags nothing while the document is hidden", () => {
    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      value: "hidden",
    });
    const entry = triggerEntry();
    const state = stateWith(entry);
    renderLayer();
    measure(7);

    commit(state, [push(entry.id)]);

    expect(document.querySelector("[data-trigger-tag]")).toBeNull();
  });

  it("tags an entry once across re-commits", () => {
    const entry = triggerEntry();
    const state = stateWith(entry);
    renderLayer();
    measure(7);

    commit(state, [push(entry.id)]);
    commit(stateWith(entry), [push(entry.id)]);

    expect(document.querySelectorAll("[data-trigger-tag]")).toHaveLength(1);
  });

  it("tags an id again once its entry has left the stack", () => {
    vi.useFakeTimers();
    const entry = triggerEntry();
    renderLayer();
    measure(7);

    commit(stateWith(entry), [push(entry.id)]);
    act(() => {
      vi.advanceTimersByTime(5000);
    });
    expect(document.querySelector("[data-trigger-tag]")).toBeNull();
    // An earlier state is restored without events, then the id is pushed again.
    act(() => {
      useGameStore.setState({
        gameState: buildGameState({ objects: buildObjectMap(source()), battlefield: [7], stack: [] }),
      });
    });
    commit(stateWith(entry), [push(entry.id)]);

    expect(document.querySelectorAll('[data-trigger-tag="80"]')).toHaveLength(1);
    vi.useRealTimers();
  });

  it("replaces a fading tag when its id is pushed again", () => {
    vi.useFakeTimers();
    const entry = triggerEntry();
    renderLayer();
    measure(7);

    commit(stateWith(entry), [push(entry.id)]);
    act(() => {
      vi.advanceTimersByTime(1000);
    });
    act(() => {
      useGameStore.setState({
        gameState: buildGameState({ objects: buildObjectMap(source()), battlefield: [7], stack: [] }),
      });
    });
    commit(stateWith(entry), [push(entry.id)]);
    expect(document.querySelectorAll('[data-trigger-tag="80"]')).toHaveLength(1);

    // Past the first tag's lifetime, short of the second's.
    act(() => {
      vi.advanceTimersByTime(1000);
    });
    expect(document.querySelectorAll('[data-trigger-tag="80"]')).toHaveLength(1);
    vi.useRealTimers();
  });

  const visionaries = [7, 8, 9, 10, 11];
  const fiveTriggers = () => visionaries.map((id, index) => triggerEntry(81 + index, id));
  const fiveOnBoard = (entries: ReturnType<typeof fiveTriggers>) =>
    buildGameState({
      objects: buildObjectMap(...visionaries.map((id) => source(id))),
      battlefield: visionaries,
      stack: entries,
    });

  it("keeps one summary across overflowing commits", () => {
    const entries = fiveTriggers();
    renderLayer(visionaries);
    visionaries.forEach((id, index) => measure(id, 100 + index * 150, 100));

    commit(fiveOnBoard(entries.slice(0, 3)), [push(81), push(82), push(83)]);
    commit(fiveOnBoard(entries.slice(0, 4)), [push(84)]);
    commit(fiveOnBoard(entries), [push(85)]);

    const summaries = document.querySelectorAll("[data-trigger-tag-more]");
    expect(summaries).toHaveLength(1);
    expect(summaries[0]?.textContent).toBe("+2 more");
  });

  it("does not count a re-pushed entry twice in the summary", () => {
    const entries = fiveTriggers();
    const pushes = entries.map((entry) => push(entry.id));
    renderLayer(visionaries);
    visionaries.forEach((id, index) => measure(id, 100 + index * 150, 100));

    commit(fiveOnBoard(entries), pushes);
    act(() => {
      useGameStore.setState({ gameState: fiveOnBoard([]) });
    });
    commit(fiveOnBoard(entries), [...pushes]);

    expect(document.querySelectorAll("[data-trigger-tag]")).toHaveLength(3);
    const summaries = document.querySelectorAll("[data-trigger-tag-more]");
    expect(summaries).toHaveLength(1);
    expect(summaries[0]?.textContent).toBe("+2 more");
  });

  it("separates tags from two sources that share one collapsed card", () => {
    const first = triggerEntry(81, 7);
    const second = triggerEntry(82, 8);
    const state = buildGameState({
      objects: buildObjectMap(source(), source(8)),
      battlefield: [7, 8],
      stack: [first, second],
    });
    renderLayer();
    measure(7);
    const card = document.querySelector('[data-object-id="7"]');
    document.querySelector('[data-object-id="8"]')?.remove();
    card?.setAttribute("data-grouped-ids", "7 8");

    commit(state, [push(first.id), push(second.id)]);

    const tops = [...document.querySelectorAll<HTMLElement>("[data-trigger-tag]")].map(
      (tag) => tag.style.top,
    );
    expect(tops).toHaveLength(2);
    expect(new Set(tops).size).toBe(2);
  });

  it("caps the tags and counts the rest in one '+N more' tag", () => {
    const entries = fiveTriggers();
    renderLayer(visionaries.slice(0, 3));
    measure(7, 100, 100);
    measure(8, 300, 100);
    measure(9, 500, 100);
    // The last two have no card to measure; they are still counted.
    for (const id of [10, 11]) {
      const anchor = document.createElement("div");
      anchor.setAttribute("data-object-id", String(id));
      document.body.appendChild(anchor);
    }

    commit(
      fiveOnBoard(entries),
      entries.map((entry) => push(entry.id)),
    );

    expect(document.querySelectorAll("[data-trigger-tag]")).toHaveLength(3);
    expect(document.querySelector('[data-trigger-tag="81"]')?.textContent).toBe(TEXT);
    expect(document.querySelector("[data-trigger-tag-more]")?.textContent).toBe("+2 more");
    for (const id of [10, 11]) document.querySelector(`body > [data-object-id="${id}"]`)?.remove();
  });

  it("clears tags and timers on a session change", () => {
    const entry = triggerEntry();
    const state = stateWith(entry);
    renderLayer();
    measure(7);
    commit(state, [push(entry.id)]);
    expect(document.querySelectorAll("[data-trigger-tag]")).toHaveLength(1);

    act(() => {
      useGameStore.setState({
        gameSessionGeneration: useGameStore.getState().gameSessionGeneration + 1,
        events: [],
      });
    });

    expect(document.querySelector("[data-trigger-tag]")).toBeNull();
  });

  it("keeps a tag inside the viewport horizontally", () => {
    const entry = triggerEntry();
    const state = stateWith(entry);
    renderLayer();
    const viewport = window.innerWidth;
    measure(7, viewport - 20, 100);

    commit(state, [push(entry.id)]);

    const tag = document.querySelector<HTMLElement>('[data-trigger-tag="80"]');
    expect(tag).not.toBeNull();
    expect(Number.parseFloat(tag!.style.left) + Number.parseFloat(tag!.style.width)).toBeLessThanOrEqual(
      viewport,
    );
  });

  it("stacks several tags on one card without sharing a top position", () => {
    // Panharmonicon: "that ability triggers an additional time", so one entering
    // Visionary puts two of its triggers on the stack.
    const first = triggerEntry(80, 7);
    const second = triggerEntry(81, 7);
    const state = buildGameState({
      objects: buildObjectMap(source()),
      battlefield: [7],
      stack: [first, second],
      derived: {
        stack_entry_details: {
          "80": { source_name: NAME, kind_label: "Triggered" },
          "81": { source_name: NAME, kind_label: "Triggered" },
        },
      },
    });
    renderLayer();
    measure(7, 400, 100);

    commit(state, [push(first.id), push(second.id)]);

    const tops = Array.from(document.querySelectorAll<HTMLElement>("[data-trigger-tag]")).map(
      (tag) => tag.style.top,
    );
    expect(tops).toHaveLength(2);
    expect(new Set(tops).size).toBe(2);
    for (const tag of document.querySelectorAll("[data-trigger-tag]")) {
      expect(tag.textContent).toBe(TEXT);
    }
  });
});
