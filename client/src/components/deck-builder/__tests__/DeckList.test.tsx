import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { GameFormat } from "../../../adapter/types";
import type { DeckCompatibilityResult } from "../../../services/deckCompatibility";
import { DeckList } from "../DeckList";

const mocks = vi.hoisted(() => ({
  canonicalCardNames: vi.fn(
    async (names: string[]): Promise<(string | null)[]> => names.map(() => null),
  ),
  inferCommander: vi.fn<typeof import("../../../services/engineRuntime").inferCommander>(),
}));

vi.mock("../../../adapter/wasm-adapter", () => ({
  getSharedAdapter: () => ({ canonicalCardNames: mocks.canonicalCardNames }),
}));

vi.mock("../../../services/engineRuntime", () => ({
  inferCommander: mocks.inferCommander,
  sideboardPolicyForFormat: async () => ({ type: "Forbidden" }),
}));

beforeEach(() => {
  mocks.canonicalCardNames.mockReset();
  mocks.canonicalCardNames.mockImplementation(async (names: string[]) => names.map(() => null));
  mocks.inferCommander.mockReset();
  mocks.inferCommander.mockResolvedValue(null);
});

afterEach(cleanup);

const emptyDeck = { main: [], sideboard: [] };

describe("DeckList commander section", () => {
  it("shows designated commanders in list view and demotes them via the remove button", () => {
    // The bug this guards: once set, a commander is filtered out of deck.main,
    // so without this pinned section it's invisible/unremovable in list view
    // (the Info-panel CommanderPanel is on a different tab on mobile).
    const onRemoveCommander = vi.fn();
    render(
      <DeckList
        deck={emptyDeck}
        onRemoveCard={vi.fn()}
        onIncrementCard={vi.fn()}
        canIncrementCard={() => true}
        onMoveCard={vi.fn()}
        onImport={vi.fn()}
        commanders={["Krenko, Mob Boss"]}
        onRemoveCommander={onRemoveCommander}
        cardDataCache={new Map()}
        groupMode="type"
      />,
    );

    expect(screen.getByText("Krenko, Mob Boss")).toBeInTheDocument();
    fireEvent.click(
      screen.getByRole("button", { name: /remove krenko, mob boss as commander/i }),
    );
    expect(onRemoveCommander).toHaveBeenCalledWith("Krenko, Mob Boss");
  });

  it("self-hides the commander section outside commander formats (no commanders)", () => {
    render(
      <DeckList
        deck={emptyDeck}
        onRemoveCard={vi.fn()}
        onIncrementCard={vi.fn()}
        canIncrementCard={() => true}
        onMoveCard={vi.fn()}
        onImport={vi.fn()}
        cardDataCache={new Map()}
        groupMode="type"
      />,
    );
    expect(screen.queryByText(/as commander/i)).not.toBeInTheDocument();
  });
});

describe("DeckList import modal", () => {
  it("shows an error and keeps the modal open when pasted text has no cards", async () => {
    const onImport = vi.fn();
    render(
      <DeckList
        deck={emptyDeck}
        onRemoveCard={vi.fn()}
        onIncrementCard={vi.fn()}
        canIncrementCard={() => true}
        onMoveCard={vi.fn()}
        onImport={onImport}
        cardDataCache={new Map()}
        groupMode="type"
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: /^import$/i }));
    fireEvent.change(screen.getByPlaceholderText(/paste deck list/i), {
      target: { value: "asdasd" },
    });
    fireEvent.click(screen.getByRole("button", { name: /^parse$/i }));

    expect(
      await screen.findByText(/couldn't find any cards/i),
    ).toBeInTheDocument();
    expect(onImport).not.toHaveBeenCalled();
    expect(screen.getByRole("button", { name: /^parse$/i })).toBeInTheDocument();
  });

  it("imports pasted card names in the engine's canonical spelling", async () => {
    mocks.canonicalCardNames.mockImplementation(async (names: string[]) =>
      names.map((name) => (name === "Revival/Revenge" ? "Revival // Revenge" : null)),
    );
    const onImport = vi.fn();
    render(
      <DeckList
        deck={emptyDeck}
        onRemoveCard={vi.fn()}
        onIncrementCard={vi.fn()}
        canIncrementCard={() => true}
        onMoveCard={vi.fn()}
        onImport={onImport}
        cardDataCache={new Map()}
        groupMode="type"
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: /^import$/i }));
    fireEvent.change(screen.getByPlaceholderText(/paste deck list/i), {
      target: { value: "1 Revival/Revenge" },
    });
    fireEvent.click(screen.getByRole("button", { name: /^parse$/i }));

    await waitFor(() => expect(onImport).toHaveBeenCalled());
    expect(onImport.mock.calls[0][0].main).toEqual([
      { count: 1, name: "Revival // Revenge" },
    ]);
  });
});

describe("DeckList commander inference on paste", () => {
  const gollum = { name: "Gollum, Silent Slinker", count: 1 };
  const sengir = { name: "Sengir Vampire", count: 1 };
  const filler = [
    { name: "Relentless Rats", count: 58 },
    { name: "Swamp", count: 40 },
  ];
  const rows = [gollum, sengir, ...filler];
  const paste = rows.map(({ count, name }) => `${count} ${name}`).join("\n");

  beforeEach(() => {
    // Same counted rows and answers as the real-WASM Commander integration.
    mocks.inferCommander.mockImplementation(async (_rows, format) =>
      format === "PauperCommander" ? sengir.name : gollum.name,
    );
  });

  function renderList(selectedFormat?: GameFormat) {
    const onImport = vi.fn();
    const list = (format?: GameFormat) => (
      <DeckList
        deck={emptyDeck}
        onRemoveCard={vi.fn()}
        onIncrementCard={vi.fn()}
        canIncrementCard={() => true}
        onMoveCard={vi.fn()}
        onImport={onImport}
        format={format ?? "Standard"}
        commanderInferenceFormat={format}
        cardDataCache={new Map()}
        groupMode="type"
      />
    );
    const { rerender } = render(list(selectedFormat));
    return { onImport, selectFormat: (format: GameFormat) => rerender(list(format)) };
  }

  function pasteCountedDeck() {
    fireEvent.click(screen.getByRole("button", { name: /^import$/i }));
    fireEvent.change(screen.getByPlaceholderText(/paste deck list/i), {
      target: { value: paste },
    });
    fireEvent.click(screen.getByRole("button", { name: /^parse$/i }));
  }

  it("imports the inferred commander for the selected format", async () => {
    const { onImport } = renderList("PauperCommander");
    pasteCountedDeck();

    await waitFor(() => expect(onImport).toHaveBeenCalledTimes(1));
    expect.soft(mocks.inferCommander.mock.calls).toEqual([[rows, "PauperCommander"]]);
    expect(onImport).toHaveBeenCalledWith(expect.objectContaining({
      commander: [sengir.name],
      main: [gollum, ...filler],
    }));
  });

  it("discovers an ordinary commander without an inference context", async () => {
    const { onImport } = renderList();
    pasteCountedDeck();

    await waitFor(() => expect(onImport).toHaveBeenCalledTimes(1));
    expect(mocks.inferCommander.mock.calls).toEqual([[rows]]);
    expect(onImport).toHaveBeenCalledWith(expect.objectContaining({
      commander: [gollum.name],
      main: [sengir, ...filler],
    }));
  });

  it("retires a pending inference when the selected format changes", async () => {
    let resolveInference!: (name: string) => void;
    mocks.inferCommander.mockReturnValueOnce(new Promise((resolve) => {
      resolveInference = resolve;
    }));
    const { onImport, selectFormat } = renderList("PauperCommander");
    pasteCountedDeck();
    await waitFor(() => expect(mocks.inferCommander.mock.calls).toEqual([[rows, "PauperCommander"]]));

    selectFormat("Commander");
    await act(async () => resolveInference(sengir.name));

    expect(onImport).not.toHaveBeenCalled();
    expect(screen.getByPlaceholderText(/paste deck list/i)).toHaveValue(paste);
    fireEvent.click(screen.getByRole("button", { name: /^parse$/i }));
    await waitFor(() => expect(onImport).toHaveBeenCalledTimes(1));
    expect(mocks.inferCommander.mock.calls).toEqual([[rows, "PauperCommander"], [rows, "Commander"]]);
    expect(onImport).toHaveBeenCalledWith(expect.objectContaining({
      commander: [gollum.name],
      main: [sengir, ...filler],
    }));
  });
});

describe("DeckList export", () => {
  it("leaves export to the deck builder toolbar", () => {
    render(
      <DeckList
        deck={{ main: [{ name: "Lightning Bolt", count: 4 }], sideboard: [] }}
        onRemoveCard={vi.fn()}
        onIncrementCard={vi.fn()}
        canIncrementCard={() => true}
        onMoveCard={vi.fn()}
        onImport={vi.fn()}
        cardDataCache={new Map()}
        groupMode="type"
      />,
    );

    expect(screen.getByRole("button", { name: "Import" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /export/i })).not.toBeInTheDocument();
  });
});

describe("DeckList unsupported card reasons", () => {
  const annexGap = {
    handler: "Misparse:wrong_player",
    source_text: "If you control a Demon, each opponent loses 2 life and you gain 2 life.",
  };
  // Verbatim census clause for Lim-Dûl's Paladin (confirmed-misparses.toml;
  // also pinned by deck_compatibility_census_clauses_survive_alias_and_composite_spellings).
  const paladinGap = {
    handler: "Misparse:missing_effect",
    source_text:
      "Whenever this creature attacks and isn't blocked, it assigns no combat damage this turn and defending player loses 4 life.",
  };

  function compatibilityResult(
    coverage: DeckCompatibilityResult["coverage"],
  ): DeckCompatibilityResult {
    return {
      standard: { compatible: true, reasons: [] },
      commander: { compatible: true, reasons: [] },
      bo3_ready: true,
      unknown_cards: [],
      selected_format_reasons: [],
      color_identity: [],
      color_distribution: [],
      coverage,
    };
  }

  const withEntryNames = compatibilityResult({
    total_unique: 6,
    supported_unique: 4,
    unsupported_cards: [
      {
        name: "Unholy Annex",
        gaps: [annexGap.handler],
        gap_details: [annexGap],
        entry_names: [
          "Unholy Annex",
          "Unholy Annex // Ritual Chamber",
          "Unholy Annex//Ritual Chamber",
          "Unholy Annex/Ritual Chamber",
        ],
      },
      {
        name: "Lim-Dûl's Paladin",
        gaps: [paladinGap.handler],
        gap_details: [paladinGap],
        entry_names: ["Lim-Dul's Paladin", "Lim-Dûl's Paladin"],
      },
    ],
  });

  function renderWithCompatibility(
    compatibility: DeckCompatibilityResult,
    names: string[],
  ): HTMLElement {
    const { container } = render(
      <DeckList
        deck={{ main: names.map((name) => ({ name, count: 1 })), sideboard: [] }}
        onRemoveCard={vi.fn()}
        onIncrementCard={vi.fn()}
        canIncrementCard={() => true}
        onMoveCard={vi.fn()}
        onImport={vi.fn()}
        compatibility={compatibility}
        cardDataCache={new Map()}
        groupMode="type"
      />,
    );
    return container;
  }

  function renderWithCommanders(
    compatibility: DeckCompatibilityResult,
    commanders: string[],
  ): HTMLElement {
    const { container } = render(
      <DeckList
        deck={emptyDeck}
        onRemoveCard={vi.fn()}
        onIncrementCard={vi.fn()}
        canIncrementCard={() => true}
        onMoveCard={vi.fn()}
        onImport={vi.fn()}
        commanders={commanders}
        compatibility={compatibility}
        cardDataCache={new Map()}
        groupMode="type"
      />,
    );
    return container;
  }

  function row(container: HTMLElement, name: string): HTMLElement {
    const element = container.querySelector(`[data-card-name="${name.toLowerCase()}"]`);
    if (!element) throw new Error(`the row for ${name} is rendered`);
    return element as HTMLElement;
  }

  function expectReasons(container: HTMLElement, name: string, reason: RegExp): void {
    const element = row(container, name);
    const expand = within(element).getByRole("button", { name: /expand details/i });
    fireEvent.click(expand);
    expect(within(element).getByText(reason)).toBeInTheDocument();
  }

  it("keys reasons by every entry spelling the engine reports", () => {
    // Glued "A//B", slash "A/B" and the accent-less alias fail without the
    // engine's entry_names: the fallback splits only the spaced separator and
    // knows only the resolved face name. The plain and spaced rows are the
    // positive controls, and the Lightning Bolt row is the reach guard that
    // the others are not rendering a reason by accident.
    const container = renderWithCompatibility(withEntryNames, [
      "Unholy Annex",
      "Unholy Annex // Ritual Chamber",
      "Unholy Annex//Ritual Chamber",
      "Unholy Annex/Ritual Chamber",
      "Lim-Dul's Paladin",
      "Lightning Bolt",
    ]);

    for (const spelling of [
      "Unholy Annex",
      "Unholy Annex // Ritual Chamber",
      "Unholy Annex//Ritual Chamber",
      "Unholy Annex/Ritual Chamber",
    ]) {
      expectReasons(container, spelling, /each opponent loses 2 life/i);
    }
    expectReasons(container, "Lim-Dul's Paladin", /assigns no combat damage/i);

    expect(
      within(row(container, "Lightning Bolt")).queryByRole("button", {
        name: /expand details/i,
      }),
    ).toBeNull();
  });

  it("still resolves the canonical spaced composite on an older engine", () => {
    // Older engines omit entry_names; 393's fallback must keep the canonical
    // "A // B" spelling working, and nothing else.
    const olderEngine = compatibilityResult({
      total_unique: 2,
      supported_unique: 1,
      unsupported_cards: [
        {
          name: "Unholy Annex",
          gaps: [annexGap.handler],
          gap_details: [annexGap],
        },
      ],
    });
    const container = renderWithCompatibility(olderEngine, [
      "Unholy Annex",
      "Unholy Annex // Ritual Chamber",
      "Unholy Annex//Ritual Chamber",
    ]);

    for (const spelling of ["Unholy Annex", "Unholy Annex // Ritual Chamber"]) {
      expectReasons(container, spelling, /each opponent loses 2 life/i);
    }
    expect(
      within(row(container, "Unholy Annex//Ritual Chamber")).queryByRole("button", {
        name: /expand details/i,
      }),
    ).toBeNull();
  });

  it("shows the reason panel on an unsupported commander row", () => {
    // The pinned commander section renders outside the main list, so it needs
    // its own lookup: without it an unsupported commander (a Room commander
    // listed by its glued spelling, or a plain-name one) shows no reason.
    const container = renderWithCommanders(withEntryNames, [
      "Unholy Annex//Ritual Chamber",
      "Lim-Dûl's Paladin",
    ]);

    expectReasons(container, "Unholy Annex//Ritual Chamber", /each opponent loses 2 life/i);
    expectReasons(container, "Lim-Dûl's Paladin", /assigns no combat damage/i);
  });

  it("leaves a supported commander row without an expand button", () => {
    // Reach guard: the unsupported commander in the same render must still
    // expand, so the absent button below is the supported card's own state.
    const container = renderWithCommanders(withEntryNames, [
      "Unholy Annex//Ritual Chamber",
      "Krenko, Mob Boss",
    ]);

    expectReasons(container, "Unholy Annex//Ritual Chamber", /each opponent loses 2 life/i);
    expect(
      within(row(container, "Krenko, Mob Boss")).queryByRole("button", {
        name: /expand details/i,
      }),
    ).toBeNull();
  });

  it("keeps a card's own reason when another card aliases its exact name", () => {
    // Two-pass unsupportedMap: every exact name is keyed before any entry_names
    // spelling, so a face name is never handed another card's gap. Lim-Dûl's
    // Paladin (B) lists Unholy Annex's exact name as an alias; with B listed
    // first, a first-write single pass over the cards would key "Unholy Annex"
    // to the Paladin. The mirrored order below pins the other naive shape,
    // last-write, which would let B's alias overwrite A when B is listed second.
    const annexCard = {
      name: "Unholy Annex",
      gaps: [annexGap.handler],
      gap_details: [annexGap],
      entry_names: ["Unholy Annex"],
    };
    const paladinCard = {
      name: "Lim-Dûl's Paladin",
      gaps: [paladinGap.handler],
      gap_details: [paladinGap],
      entry_names: ["Unholy Annex", "Lim-Dul's Paladin", "Lim-Dûl's Paladin"],
    };
    const names = ["Unholy Annex", "Lim-Dûl's Paladin"];

    const paladinFirst = renderWithCompatibility(
      compatibilityResult({
        total_unique: 2,
        supported_unique: 0,
        unsupported_cards: [paladinCard, annexCard],
      }),
      names,
    );
    expectReasons(paladinFirst, "Unholy Annex", /each opponent loses 2 life/i);
    expectReasons(paladinFirst, "Lim-Dûl's Paladin", /assigns no combat damage/i);

    const annexFirst = renderWithCompatibility(
      compatibilityResult({
        total_unique: 2,
        supported_unique: 0,
        unsupported_cards: [annexCard, paladinCard],
      }),
      names,
    );
    expectReasons(annexFirst, "Unholy Annex", /each opponent loses 2 life/i);
    expectReasons(annexFirst, "Lim-Dûl's Paladin", /assigns no combat damage/i);
  });
});
