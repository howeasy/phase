import { useId } from "react";
import { useTranslation } from "react-i18next";

import type { UnsupportedCard } from "../../services/deckCompatibility";

/**
 * Engine-authored gap list for one unsupported card, under a translated
 * heading. Handler labels and clause text are engine pass-through (i18n
 * boundary rule: raw, no `t()`); only the heading is frontend chrome.
 *
 * Renders `gap_details` (handler label + printed clause) when the engine
 * sent it; falls back to the plain `gaps` handler-label strings otherwise.
 * Returns null when neither carries any entry.
 */
export function UnsupportedGapList({ card }: { card: UnsupportedCard }) {
  const { t } = useTranslation("deck-builder");
  const headingId = useId();
  const details = card.gap_details ?? [];
  if (details.length === 0 && card.gaps.length === 0) return null;
  return (
    <div className="mb-1.5">
      <div
        id={headingId}
        className="mb-1 text-[9px] font-semibold uppercase tracking-wider text-amber-300"
      >
        {t("card.unsupportedReasonsHeading")}
      </div>
      <ul
        data-testid="unsupported-gap-list"
        aria-labelledby={headingId}
        className="flex flex-col gap-1 break-words"
      >
        {details.length > 0
          ? details.map((gap, i) => (
              <li key={i} className="break-words">
                <span className="mr-1 inline-block rounded bg-rose-500/15 px-1 py-px font-mono text-[9px] font-semibold leading-tight text-rose-300">
                  {gap.handler}
                </span>
                {gap.source_text && (
                  <span className="text-[11px] leading-snug text-amber-100/90">{gap.source_text}</span>
                )}
              </li>
            ))
          : card.gaps.map((gap, i) => (
              <li key={i} className="break-words text-[11px] leading-snug text-amber-100/90">
                {gap}
              </li>
            ))}
      </ul>
    </div>
  );
}
