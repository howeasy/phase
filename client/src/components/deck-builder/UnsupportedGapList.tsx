import type { UnsupportedCard } from "../../services/deckCompatibility";

/**
 * Engine-authored gap list for one unsupported card. Handler labels and
 * clause text are engine pass-through (i18n boundary rule: raw, no `t()`);
 * only the surrounding chrome is translated by the caller.
 *
 * Renders `gap_details` (handler label + printed clause) when the engine
 * sent it; falls back to the plain `gaps` handler-label strings otherwise.
 * Returns null when neither carries any entry.
 */
export function UnsupportedGapList({ card }: { card: UnsupportedCard }) {
  const details = card.gap_details ?? [];
  if (details.length > 0) {
    return (
      <ul data-testid="unsupported-gap-list" className="flex flex-col gap-1 break-words">
        {details.map((gap, i) => (
          <li key={i} className="break-words">
            <span className="mr-1 inline-block rounded bg-rose-500/15 px-1 py-px font-mono text-[9px] font-semibold leading-tight text-rose-300">
              {gap.handler}
            </span>
            {gap.source_text && (
              <span className="text-[11px] leading-snug text-amber-100/90">{gap.source_text}</span>
            )}
          </li>
        ))}
      </ul>
    );
  }
  if (card.gaps.length > 0) {
    return (
      <ul data-testid="unsupported-gap-list" className="flex flex-col gap-1 break-words">
        {card.gaps.map((gap, i) => (
          <li key={i} className="break-words text-[11px] leading-snug text-amber-100/90">
            {gap}
          </li>
        ))}
      </ul>
    );
  }
  return null;
}
