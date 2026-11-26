/** The header of a detail page: how its `detail` actions group into buttons, named menus and "More". */
export interface Groupable {
  name: string;
  menu?: string | null;
}

export interface Groups<T extends Groupable> {
  /** The actions with no `menu`, up to `MAX_BUTTONS`: one button each. */
  buttons: T[];
  /** One dropdown for each `menu` text, in the order of its first action. */
  menus: { label: string; items: T[] }[];
  /** The buttons over the limit. The built-in items of "More" come after them. */
  more: T[];
}

export const MAX_BUTTONS = 3;

export function groupActions<T extends Groupable>(actions: readonly T[]): Groups<T> {
  const plain = actions.filter((a) => !a.menu);
  const menus: Groups<T>["menus"] = [];
  for (const a of actions) {
    if (!a.menu) continue;
    const found = menus.find((m) => m.label === a.menu);
    if (found) found.items.push(a);
    else menus.push({ label: a.menu, items: [a] });
  }
  return { buttons: plain.slice(0, MAX_BUTTONS), menus, more: plain.slice(MAX_BUTTONS) };
}
