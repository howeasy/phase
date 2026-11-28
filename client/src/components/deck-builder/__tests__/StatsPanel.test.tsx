import { describe, expect, it, vi } from "vitest";
import { render } from "@testing-library/react";

import { StatsPanel } from "../StatsPanel";

describe("StatsPanel", () => {
  it("owns one mana curve followed by one color distribution", () => {
    const { container } = render(
      <StatsPanel
        compatibility={null}
        cmcValues={[1, 2]}
        colorDistribution={[
          { color: "White", count: 1, percentage: 50, display_percentage: 50 },
          { color: "Blue", count: 1, percentage: 50, display_percentage: 50 },
        ]}
        isCommander={false}
        estimate={null}
        signals={null}
        manualBracket={null}
        onBracketChange={vi.fn()}
        comboDeclaration={{ kind: "undeclared" }}
        onComboDeclarationChange={vi.fn()}
        onCardClick={vi.fn()}
      />,
    );

    const analysis = container.querySelector("[data-stats-panel-analysis]")!;
    expect(analysis.querySelectorAll("[data-mana-curve]")).toHaveLength(1);
    expect(analysis.querySelectorAll("[data-color-distribution]")).toHaveLength(1);
    const curve = analysis.querySelector("[data-mana-curve]")!;
    const colors = analysis.querySelector("[data-color-distribution]")!;
    expect(curve.compareDocumentPosition(colors) & Node.DOCUMENT_POSITION_FOLLOWING)
      .not.toBe(0);
    expect(curve.querySelector("[data-color-distribution]")).toBeNull();
  });

  it("names the printed clause behind each unsupported card in the coverage tooltip", () => {
    const clause =
      "Whenever Zur attacks, you may search your library for an enchantment card with mana value 3 or less, put it onto the battlefield, then shuffle.";
    const { container } = render(
      <StatsPanel
        compatibility={{
          standard: { compatible: true, reasons: [] },
          commander: { compatible: true, reasons: [] },
          bo3_ready: false,
          unknown_cards: [],
          selected_format_reasons: [],
          color_identity: [],
          color_distribution: [],
          coverage: {
            total_unique: 2,
            supported_unique: 1,
            unsupported_cards: [
              {
                name: "Zur the Enchanter",
                gaps: ["Misparse:wrong_object"],
                gap_details: [{ handler: "Misparse:wrong_object", source_text: clause }],
              },
            ],
          },
        }}
        cmcValues={[]}
        colorDistribution={[]}
        isCommander={false}
        estimate={null}
        signals={null}
        manualBracket={null}
        onBracketChange={vi.fn()}
        comboDeclaration={{ kind: "undeclared" }}
        onComboDeclarationChange={vi.fn()}
        onCardClick={vi.fn()}
      />,
    );

    const ratio = container.querySelector('[title^="Unsupported:"]')!;
    expect(ratio.getAttribute("title")).toBe(
      `Unsupported:\nZur the Enchanter: ${clause} [Misparse:wrong_object]`,
    );
  });
});
