// ─── Card VFX presenter ───
// The thin seam over `CardVfxLayer.present`. The layer is looked up through a
// ref on every call, never captured at render time, so a mount, loss or failed
// start between renders is seen by the next effect.

import type { CardVfxLayerHandle } from "../../components/animation/cardVfx/CardVfxLayer.tsx";
import type { CardVfxPresenter } from "./types.ts";

export function createCardVfxPresenter(
  getLayer: () => CardVfxLayerHandle | null,
): CardVfxPresenter {
  return {
    get available() {
      return getLayer() !== null;
    },

    present(spec, classic, onImpact) {
      const layer = getLayer();
      if (!layer) {
        classic();
        return;
      }
      layer.present(spec, classic, onImpact);
    },

    /** Something a slam's impact lands under the New style: the layer's dust
     *  and ring, or the struck card's knockback. Presented as the slam starts;
     *  returns what runs as the slam lands, which runs `classic` if the layer
     *  could not present it. A layer that gives up after the impact runs it then. */
    presentBlow(spec, classic) {
      const layer = getLayer();
      if (!layer) return classic;
      let fellBack = false;
      let landed = false;
      layer.present(spec, () => {
        fellBack = true;
        if (landed) classic();
      });
      return () => {
        landed = true;
        if (fellBack) classic();
      };
    },
  };
}
