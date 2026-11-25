// The rules for the sign-in that starts with no form (`auto_start`): when the page goes to the provider, and where the user returns.

/** A start that happened less than this long ago did not give a session: the page shows the form, not a loop. */
export const LOOP_WINDOW_MS = 15_000;

export interface AutoStart {
  /** The login state: the start address and the `auto_start` flag of the site. */
  sso: string | null;
  autoStart: boolean;
  /** The page was opened with `sso_error` or `password=1`: a person asks for the form. */
  formOnly: boolean;
  /** When the page last went to the provider (ms since the epoch), or null. */
  lastStart: number | null;
  now: number;
}

/** True when a visitor with no session goes to the provider at once. */
export function shouldAutoStart(a: AutoStart): boolean {
  if (!a.autoStart || a.sso === null || a.formOnly) return false;
  return a.lastStart === null || a.now - a.lastStart > LOOP_WINDOW_MS || a.now < a.lastStart;
}

/** The start address with the page the user asked for: the server checks that it stays under the base path. */
export function startUrl(sso: string, pathname: string, search: string): string {
  return `${sso}?next=${encodeURIComponent(pathname + search)}`;
}

/** Whether the query asks for the sign-in form: a failed callback or the break-glass address. */
export function wantsForm(search: string): boolean {
  const q = new URLSearchParams(search);
  return q.has("sso_error") || q.get("password") === "1";
}
