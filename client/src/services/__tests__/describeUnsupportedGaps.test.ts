import { describe, expect, it } from "vitest";

import { describeUnsupportedGaps, type UnsupportedCard } from "../deckCompatibility";

const card = (overrides: Partial<UnsupportedCard>): UnsupportedCard => ({
  name: "Lightning Bolt",
  gaps: [],
  ...overrides,
});

const ZUR_CLAUSE =
  "Whenever Zur attacks, you may search your library for an enchantment card with mana value 3 or less, put it onto the battlefield, then shuffle.";

describe("describeUnsupportedGaps", () => {
  it("joins gap_details entries as 'clause [handler]'", () => {
    expect(
      describeUnsupportedGaps(
        card({
          gaps: ["Misparse:wrong_object"],
          gap_details: [{ handler: "Misparse:wrong_object", source_text: ZUR_CLAUSE }],
        }),
      ),
    ).toBe(`${ZUR_CLAUSE} [Misparse:wrong_object]`);
  });

  it("uses just [handler] when an entry has no source_text", () => {
    expect(
      describeUnsupportedGaps(
        card({
          gaps: ["Effect:Unimplemented"],
          gap_details: [{ handler: "Effect:Unimplemented" }],
        }),
      ),
    ).toBe("[Effect:Unimplemented]");
  });

  it("joins several gap_details entries with '; '", () => {
    expect(
      describeUnsupportedGaps(
        card({
          gaps: ["Effect:Unimplemented", "Trigger:ChangesZone", "SilentDrop"],
          gap_details: [
            { handler: "Effect:Unimplemented", source_text: "Lightning Bolt deals 3 damage to any target." },
            { handler: "Trigger:ChangesZone" },
            { handler: "SilentDrop" },
          ],
        }),
      ),
    ).toBe(
      "Lightning Bolt deals 3 damage to any target. [Effect:Unimplemented]; [Trigger:ChangesZone]; [SilentDrop]",
    );
  });

  it("falls back to gaps when gap_details is absent", () => {
    expect(
      describeUnsupportedGaps(card({ gaps: ["DealDamage.PlayerOnly", "Effect:Unimplemented"] })),
    ).toBe("DealDamage.PlayerOnly, Effect:Unimplemented");
  });

  it("falls back to gaps when gap_details is empty (older engines can omit entries)", () => {
    expect(describeUnsupportedGaps(card({ gaps: ["DealDamage.PlayerOnly"], gap_details: [] }))).toBe(
      "DealDamage.PlayerOnly",
    );
  });

  it("returns an empty string when both are empty", () => {
    expect(describeUnsupportedGaps(card({ gaps: [], gap_details: [] }))).toBe("");
  });
});
