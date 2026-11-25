import { useEffect, useState, type ReactNode } from "react";
import { ChevronDown, ChevronRight, House, Search, Users } from "lucide-react";
import * as api from "./api";
import { Input } from "./components/ui/input";
import { NavIcon } from "./icons";
import { cn } from "./lib/utils";
import { BASE, Link, listUrl } from "./router";

/** The group of the built-in pages: `GROUP` in crates/admin/src/site/builtin.rs. The e2e test checks that it has its icon. */
const USERS_AND_ACCESS = "Users and access";

export function Sidebar({ site, open, current, home, onNavigate, header, footer }: { site: api.Site; open: boolean; current: string | null; home: boolean; onNavigate: () => void; header: ReactNode; footer: ReactNode }) {
  const [filter, setFilter] = useState("");
  const [shut, setShut] = useState<Set<string>>(new Set());
  const byName = new Map(site.resources.map((r) => [r.name, r]));
  const title = (n: string) => byName.get(n)?.title ?? n;
  const needle = filter.trim().toLowerCase();
  const groups = site.nav
    .map((g) => ({ group: g.group, icon: g.icon, names: g.resources.filter((n) => !needle || title(n).toLowerCase().includes(needle) || n.toLowerCase().includes(needle)) }))
    .filter((g) => g.names.length > 0);
  // The group of the page that opens is shown once; after that the user's choice holds. The sidebar stays mounted when it is hidden, so the filter and this state stay.
  useEffect(() => {
    const own = current === null ? undefined : site.nav.find((g) => g.resources.includes(current));
    if (own) setShut((s) => (s.has(own.group ?? "") ? new Set([...s].filter((k) => k !== (own.group ?? ""))) : s));
  }, [current, site]);
  const flip = (key: string) => setShut((s) => (s.has(key) ? new Set([...s].filter((k) => k !== key)) : new Set([...s, key])));
  return (
    <aside id="sidebar" hidden={!open} className="w-64 shrink-0 border-r border-border bg-card max-md:fixed max-md:inset-y-0 max-md:z-40 max-md:shadow-lg">
      {/* The aside stretches to the full height of the page so its background reaches the bottom; this part sticks to the top. */}
      <div className="sticky top-0 flex h-screen flex-col max-md:static max-md:h-full">
        <div className="border-b border-border p-3">{header}</div>
        <div className="min-h-0 flex-1 overflow-y-auto p-3">
        <div className="relative mb-3">
          <Search className="pointer-events-none absolute top-2.5 left-2.5 size-4 text-muted-foreground" aria-hidden />
          <kbd className="pointer-events-none absolute top-2 right-2 rounded border border-border px-1 font-sans text-[10px] text-muted-foreground">{SHORTCUT}</kbd>
          <Input type="search" id="sidebar-search" value={filter} onChange={(e) => setFilter(e.target.value)} placeholder="Search apps and models…" aria-label="Filter the resources" className="pr-12 pl-8" />
        </div>
        <nav aria-label="Resources">
          <Link href={BASE} aria-current={home ? "page" : undefined} className={cn(item, "mb-2", home && "bg-accent font-medium text-accent-foreground")} onClick={onNavigate}>
            <House className="size-4" aria-hidden />
            Home
          </Link>
          {groups.map((g) => {
            const key = g.group ?? "";
            const expanded = !!needle || !shut.has(key);
            const Icon = expanded ? ChevronDown : ChevronRight;
            return (
              <div key={key} className="mb-2">
                {g.group && (
                  <button type="button" onClick={() => flip(key)} aria-expanded={expanded} className="flex w-full items-center gap-1 rounded-md px-2 py-1.5 text-left text-xs font-semibold tracking-wide text-muted-foreground uppercase hover:text-foreground">
                    <Icon className="size-3.5" aria-hidden />
                    {(g.icon || g.group === USERS_AND_ACCESS) && <NavIcon name={g.icon} fallback={Users} className="size-3.5" />}
                    {g.group}
                  </button>
                )}
                {expanded && (
                  <ul className="m-0 list-none p-0">
                    {g.names.map((n) => (
                      <li key={n}>
                        <Link href={listUrl(n)} aria-current={current === n ? "page" : undefined} onClick={onNavigate} className={cn(item, current === n && "bg-accent font-medium text-accent-foreground")}>
                          <NavIcon name={byName.get(n)?.icon} className="size-4 shrink-0 text-muted-foreground" />
                          {title(n)}
                        </Link>
                      </li>
                    ))}
                  </ul>
                )}
              </div>
            );
          })}
          {needle && groups.length === 0 && <p className="px-2 text-sm text-muted-foreground">No resource matches.</p>}
        </nav>
        </div>
        <div className="border-t border-border p-2">{footer}</div>
      </div>
    </aside>
  );
}

const SHORTCUT = /Mac|iPhone|iPad/.test(navigator.platform) ? "⌘K" : "Ctrl K";

const item = "flex items-center gap-2 rounded-md px-2 py-1.5 text-sm text-foreground no-underline hover:bg-accent";
