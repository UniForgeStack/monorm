/** How a list or detail cell shows a value as an icon or a badge. Pure: the components draw what this returns. */

export const BADGE_COLOURS = ["grey", "blue", "green", "yellow", "red", "purple"] as const;
export type BadgeColour = (typeof BADGE_COLOURS)[number];

export interface DisplayColumn {
  widget: string;
  array: boolean;
  /** The value, as text, to a colour name. */
  badge?: Record<string, string>;
}

export type Shown =
  | { kind: "icon"; icon: "check" | "x" | "dash"; label: "Yes" | "No" | "Empty" }
  | { kind: "badge"; colour: BadgeColour; text: string };

const COLOURS = new Set<string>(BADGE_COLOURS);

/** The key of a value in a badge map: an int as decimal text, a bool as `true` or `false`. */
const keyOf = (value: unknown): string => String(value);

/**
 * What a cell shows besides plain text, or null for plain text.
 * A bool column is an icon in a list (`icons`), unless the column has a badge. A null value has no badge.
 * A value outside the badge map gets the grey badge.
 */
export function shown(value: unknown, column: DisplayColumn, icons: boolean): Shown | null {
  if (column.array || Array.isArray(value)) return null;
  const bool = column.widget === "checkbox";
  if (column.badge) {
    if (value === null || value === undefined) return null;
    const named = column.badge[keyOf(value)];
    return { kind: "badge", colour: named !== undefined && COLOURS.has(named) ? (named as BadgeColour) : "grey", text: bool ? (value ? "Yes" : "No") : String(value) };
  }
  if (!icons || !bool) return null;
  if (value === null || value === undefined) return { kind: "icon", icon: "dash", label: "Empty" };
  return value ? { kind: "icon", icon: "check", label: "Yes" } : { kind: "icon", icon: "x", label: "No" };
}
