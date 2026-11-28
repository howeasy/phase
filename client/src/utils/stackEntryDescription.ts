import type { StackEntry, StackEntryDisplay } from "../adapter/types.ts";
import { renderDescription } from "./description.ts";

/**
 * The ability description a stack entry presents, as one rule both the stack
 * and the battlefield trigger tag read. The engine-derived display row wins;
 * the entry's own captured description is the fallback. `sourceName` is the
 * row's pre-resolved source name, so `~` (CR 201.5) reads the same on every
 * surface. A missing or empty description yields `undefined`: what the source
 * object printed is never borrowed.
 */
export function stackEntryDescription(
  entry: StackEntry,
  details: StackEntryDisplay | undefined,
  sourceName: string,
): string | undefined {
  if (details?.ability_description) {
    return renderDescription(details.ability_description, sourceName);
  }
  if (entry.kind.type === "TriggeredAbility" && entry.kind.data.description) {
    return renderDescription(entry.kind.data.description, sourceName);
  }
  return undefined;
}
