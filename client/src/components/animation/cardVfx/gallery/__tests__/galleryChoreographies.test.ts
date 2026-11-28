import { describe, expect, it } from "vitest";

import { normalizeEvents } from "../../../../../animation/eventNormalizer.ts";
import { GALLERY_SCENARIOS, galleryBoard } from "../galleryScenarios.ts";

/** The effect kinds a scenario's batches reach the overlay as. */
function presentedKinds(name: string): Set<string> {
  const state = galleryBoard();
  return new Set(
    GALLERY_SCENARIOS[name].batches.flatMap((batch) =>
      normalizeEvents(batch(state)).flatMap((step) => step.effects.map((effect) => effect.event.type)),
    ),
  );
}

// A scenario that stops short of its choreography still plays, so a story can
// look healthy while showing something else.
describe("gallery stories reach their choreography", () => {
  it.each([
    ["meld", "Melded"],
    ["cardsRevealed", "CardsRevealed"],
    ["damageFlurry", "GroupedDamageFlurry"],
  ])("%s presents %s", (name, kind) => {
    expect(presentedKinds(name)).toContain(kind);
  });
});
