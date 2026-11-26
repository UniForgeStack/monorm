/** The fold state of a detail section: kept in `localStorage` for each resource and section, so a reload and the next record keep it. */
const key = (resource: string, section: string) => `monorm-admin-fold:${resource}:${section}`;

/** True when the section is open. A section with no stored state starts open, or folded when `collapsed` is set. */
export function readOpen(resource: string, section: string, collapsed: boolean): boolean {
  try {
    const stored = localStorage.getItem(key(resource, section));
    if (stored === "open") return true;
    if (stored === "folded") return false;
  } catch {
    // A browser with no storage keeps the default.
  }
  return !collapsed;
}

export function writeOpen(resource: string, section: string, open: boolean) {
  try {
    localStorage.setItem(key(resource, section), open ? "open" : "folded");
  } catch {
    // The state lasts for the page only.
  }
}
