import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { CardEntryRow } from "../CardEntryRow";

afterEach(cleanup);

const ZUR_CLAUSE =
  "Whenever Zur attacks, you may search your library for an enchantment card with mana value 3 or less, put it onto the battlefield, then shuffle.";
const ZUR_ORACLE = `Flying\n${ZUR_CLAUSE}`;
const BOLT_TEXT = "Lightning Bolt deals 3 damage to any target.";

const entry = { count: 3, name: "Lightning Bolt" };

describe("CardEntryRow", () => {
  it("renders entry name and count", () => {
    render(
      <CardEntryRow entry={entry} section="main" onMove={vi.fn()} onRemove={vi.fn()} />,
    );
    expect(screen.getByText("Lightning Bolt")).toBeInTheDocument();
    expect(screen.getByText("3x")).toBeInTheDocument();
  });

  it("fires onMove with (entry, section) when the arrow button is clicked", () => {
    const onMove = vi.fn();
    render(
      <CardEntryRow entry={entry} section="main" onMove={onMove} onRemove={vi.fn()} />,
    );
    fireEvent.click(screen.getByRole("button", { name: /move one lightning bolt to sideboard/i }));
    expect(onMove).toHaveBeenCalledWith(entry, "main");
  });

  it("forwards a row's selected printing to the hover preview", () => {
    const onCardHover = vi.fn();
    render(
      <CardEntryRow
        entry={{
          ...entry,
          sourcePrinting: { setCode: "lea", collectorNumber: "161" },
        }}
        section="main"
        onMove={vi.fn()}
        onCardHover={onCardHover}
      />,
    );

    fireEvent.click(screen.getByText("Lightning Bolt"));

    expect(onCardHover).toHaveBeenCalledWith({
      name: "Lightning Bolt",
      sourcePrinting: { setCode: "lea", collectorNumber: "161" },
    });
  });

  it("shows an arrow pointing the opposite direction for sideboard rows", () => {
    const onMove = vi.fn();
    render(
      <CardEntryRow entry={entry} section="sideboard" onMove={onMove} onRemove={vi.fn()} />,
    );
    fireEvent.click(screen.getByRole("button", { name: /move one lightning bolt to main deck/i }));
    expect(onMove).toHaveBeenCalledWith(entry, "sideboard");
  });

  it("renders a labelled move button naming the destination when moveTargetLabel is given", () => {
    const onMove = vi.fn();
    render(
      <CardEntryRow
        entry={entry}
        section="main"
        onMove={onMove}
        onRemove={vi.fn()}
        moveTargetLabel="Maybeboard"
      />,
    );
    // The destination is on the visible label (not just the tooltip) so it's
    // legible on touch where the title/aria tooltip never appears.
    const button = screen.getByRole("button", { name: /move one lightning bolt to maybeboard/i });
    expect(button).toHaveTextContent("Maybeboard");
    fireEvent.click(button);
    expect(onMove).toHaveBeenCalledWith(entry, "main");
  });

  it("fires onRemove with (entry, section) when the remove button is clicked", () => {
    const onRemove = vi.fn();
    render(
      <CardEntryRow entry={entry} section="main" onMove={vi.fn()} onRemove={onRemove} />,
    );
    fireEvent.click(screen.getByRole("button", { name: /remove one lightning bolt/i }));
    expect(onRemove).toHaveBeenCalledWith(entry, "main");
  });

  it("omits the remove button when onRemove is not provided (partition-only mode)", () => {
    render(<CardEntryRow entry={entry} section="main" onMove={vi.fn()} />);
    expect(
      screen.queryByRole("button", { name: /remove one lightning bolt/i }),
    ).not.toBeInTheDocument();
    // Move button still present so the row is functional in partition mode.
    expect(
      screen.getByRole("button", { name: /move one lightning bolt to sideboard/i }),
    ).toBeInTheDocument();
  });

  it("fires onIncrement with (entry, section) when the add button is clicked", () => {
    const onIncrement = vi.fn();
    render(
      <CardEntryRow
        entry={entry}
        section="sideboard"
        onMove={vi.fn()}
        onRemove={vi.fn()}
        onIncrement={onIncrement}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /add one lightning bolt/i }));
    expect(onIncrement).toHaveBeenCalledWith(entry, "sideboard");
  });

  // CR 100.2a: the ceiling is engine-resolved; the row only reflects it.
  it("disables the add button when the card is at its copy limit", () => {
    const onIncrement = vi.fn();
    render(
      <CardEntryRow
        entry={entry}
        section="main"
        onMove={vi.fn()}
        onRemove={vi.fn()}
        onIncrement={onIncrement}
        canIncrement={() => false}
      />,
    );
    const button = screen.getByRole("button", { name: /at the copy limit/i });
    expect(button).toBeDisabled();
    fireEvent.click(button);
    expect(onIncrement).not.toHaveBeenCalled();
  });

  it("omits the add button when onIncrement is not provided (partition-only mode)", () => {
    render(<CardEntryRow entry={entry} section="main" onMove={vi.fn()} onRemove={vi.fn()} />);
    expect(
      screen.queryByRole("button", { name: /add one lightning bolt/i }),
    ).not.toBeInTheDocument();
  });

  it("renders an unsupported-mechanics badge with expandable details", () => {
    render(
      <CardEntryRow
        entry={entry}
        section="main"
        onMove={vi.fn()}
        onRemove={vi.fn()}
        unsupported={{
          name: "Lightning Bolt",
          gaps: ["DealDamage.PlayerOnly"],
          copies: 3,
          oracle_text: "Lightning Bolt deals 3 damage to any target.",
          parse_details: [],
        }}
      />,
    );
    const badge = screen.getByRole("button", { name: /1 unsupported mechanic/i });
    expect(badge).toHaveAttribute("aria-expanded", "false");
    fireEvent.click(badge);
    expect(badge).toHaveAttribute("aria-expanded", "true");
    expect(
      screen.getByText("Lightning Bolt deals 3 damage to any target."),
    ).toBeInTheDocument();
  });

  it("shows why a card is unsupported above its oracle text when gap_details is sent", () => {
    render(
      <CardEntryRow
        entry={{ count: 1, name: "Zur the Enchanter" }}
        section="main"
        onMove={vi.fn()}
        onRemove={vi.fn()}
        unsupported={{
          name: "Zur the Enchanter",
          gaps: ["Misparse:wrong_object"],
          copies: 1,
          oracle_text: ZUR_ORACLE,
          parse_details: [{ category: "keyword", label: "Flying", supported: true }],
          gap_details: [{ handler: "Misparse:wrong_object", source_text: ZUR_CLAUSE }],
        }}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /1 unsupported mechanic/i }));
    const heading = screen.getByText("Why this card is unsupported");
    const clause = screen.getByText(ZUR_CLAUSE);
    const handler = screen.getByText("Misparse:wrong_object");
    // DOM order: heading and gap detail precede the oracle text.
    const oracle = screen.getByText(`Flying ${ZUR_CLAUSE}`);
    expect(heading.compareDocumentPosition(oracle) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(clause.compareDocumentPosition(oracle) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(handler.compareDocumentPosition(oracle) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    expect(screen.getByTestId("unsupported-gap-list").children).toHaveLength(1);
  });

  it("renders one list item per gap kind when several gaps are reported", () => {
    render(
      <CardEntryRow
        entry={{ count: 1, name: "Zur the Enchanter" }}
        section="main"
        onMove={vi.fn()}
        onRemove={vi.fn()}
        unsupported={{
          name: "Zur the Enchanter",
          gaps: ["Effect:Unimplemented", "Trigger:ChangesZone", "SilentDrop"],
          copies: 1,
          oracle_text: ZUR_ORACLE,
          parse_details: [],
          gap_details: [
            { handler: "Effect:Unimplemented", source_text: ZUR_CLAUSE },
            { handler: "Trigger:ChangesZone" },
            { handler: "SilentDrop" },
          ],
        }}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /3 unsupported mechanics/i }));
    expect(screen.getByTestId("unsupported-gap-list").children).toHaveLength(3);
    expect(screen.getByText("Effect:Unimplemented")).toBeInTheDocument();
    expect(screen.getByText("Trigger:ChangesZone")).toBeInTheDocument();
    expect(screen.getByText("SilentDrop")).toBeInTheDocument();
  });

  it("renders six list items for six gap kinds", () => {
    const handlers = ["Effect:Unimplemented", "Trigger:ChangesZone", "SilentDrop", "Misparse:wrong_object", "Cost:Unpaid", "Static:ChangesLayer"];
    render(
      <CardEntryRow
        entry={{ count: 2, name: "Lightning Bolt" }}
        section="main"
        onMove={vi.fn()}
        onRemove={vi.fn()}
        unsupported={{
          name: "Lightning Bolt",
          gaps: handlers,
          copies: 2,
          oracle_text: BOLT_TEXT,
          parse_details: [],
          gap_details: handlers.map((handler) => ({ handler })),
        }}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /6 unsupported mechanics/i }));
    expect(screen.getByTestId("unsupported-gap-list").children).toHaveLength(6);
  });

  it("falls back to the plain gaps strings when gap_details is absent", () => {
    render(
      <CardEntryRow
        entry={entry}
        section="main"
        onMove={vi.fn()}
        onRemove={vi.fn()}
        unsupported={{
          name: "Lightning Bolt",
          gaps: ["DealDamage.PlayerOnly", "Effect:Unimplemented"],
          copies: 3,
          oracle_text: BOLT_TEXT,
          parse_details: [],
        }}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /2 unsupported mechanics/i }));
    const list = screen.getByTestId("unsupported-gap-list");
    expect(list.children).toHaveLength(2);
    expect(screen.getByText("DealDamage.PlayerOnly")).toBeInTheDocument();
    expect(screen.getByText("Effect:Unimplemented")).toBeInTheDocument();
  });

  it("renders no heading or list when both gaps and gap_details are empty", () => {
    render(
      <CardEntryRow
        entry={entry}
        section="main"
        onMove={vi.fn()}
        onRemove={vi.fn()}
        unsupported={{
          name: "Lightning Bolt",
          gaps: [],
          copies: 3,
          oracle_text: BOLT_TEXT,
          parse_details: [],
        }}
      />,
    );
    // The badge itself still renders and expands.
    const badge = screen.getByRole("button", { name: /0 unsupported mechanics/i });
    fireEvent.click(badge);
    expect(badge).toHaveAttribute("aria-expanded", "true");
    expect(screen.queryByText("Why this card is unsupported")).not.toBeInTheDocument();
    expect(screen.queryByTestId("unsupported-gap-list")).not.toBeInTheDocument();
    expect(screen.getByText(BOLT_TEXT)).toBeInTheDocument();
  });

  it("marks unsupported parse chips with data-supported=false and a named mark", () => {
    render(
      <CardEntryRow
        entry={{ count: 1, name: "Zur the Enchanter" }}
        section="main"
        onMove={vi.fn()}
        onRemove={vi.fn()}
        unsupported={{
          name: "Zur the Enchanter",
          gaps: ["Misparse:wrong_object"],
          copies: 1,
          oracle_text: ZUR_ORACLE,
          parse_details: [
            { category: "keyword", label: "Flying", supported: true },
            { category: "trigger", label: "SearchLibrary", supported: false, source_text: ZUR_CLAUSE },
          ],
          gap_details: [{ handler: "Misparse:wrong_object", source_text: ZUR_CLAUSE }],
        }}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /1 unsupported mechanic/i }));
    const chips = document.querySelectorAll("[data-supported]");
    expect(chips).toHaveLength(2);
    const unsupportedChip = document.querySelector('[data-supported="false"]');
    const supportedChip = document.querySelector('[data-supported="true"]');
    expect(unsupportedChip).not.toBeNull();
    expect(supportedChip).not.toBeNull();
    expect(unsupportedChip?.querySelector('[role="img"]')).toHaveAttribute("aria-label", "Unsupported");
    expect(supportedChip?.querySelector('[role="img"]')).toBeNull();
  });

  it("keeps the badge title and aria-label counting gaps", () => {
    render(
      <CardEntryRow
        entry={entry}
        section="main"
        onMove={vi.fn()}
        onRemove={vi.fn()}
        unsupported={{
          name: "Lightning Bolt",
          gaps: ["DealDamage.PlayerOnly", "Effect:Unimplemented", "Trigger:ChangesZone"],
          copies: 3,
          oracle_text: BOLT_TEXT,
          parse_details: [],
        }}
      />,
    );
    const badge = screen.getByRole("button", { name: "3 unsupported mechanics; expand details" });
    expect(badge).toHaveAttribute("aria-label", "3 unsupported mechanics; expand details");
    expect(badge).toHaveAttribute("title", "3 unsupported mechanics — click to expand");
  });
});
