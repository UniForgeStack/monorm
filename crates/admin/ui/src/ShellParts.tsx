import { useEffect, useMemo, useState } from "react";
import { ChevronsUpDown, LayoutDashboard, Search } from "lucide-react";
import type * as api from "./api";
import { Dialog, DialogContent, DialogTitle } from "./components/ui/dialog";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger } from "./components/ui/dropdown-menu";
import { NavIcon } from "./icons";
import { envLevel } from "./lib/ramp";
import { cn } from "./lib/utils";
import { BASE, Link, listUrl, navigate } from "./router";

/** The tile, the title and the build subheader of the sidebar header. With a switcher it opens the menu of the services. */
export function ServiceSwitcher({ brand }: { brand: api.Brand }) {
  const sw = brand.switcher;
  const current = sw?.services.find((s) => s.key === sw.current);
  const head = (
    <>
      <span className="grid size-9 shrink-0 place-items-center rounded-md bg-primary text-primary-foreground">
        {brand.logo ? <img src={brand.logo} alt="" className="size-6 object-contain" /> : <NavIcon name={current?.icon} fallback={LayoutDashboard} className="size-5" />}
      </span>
      <span className="min-w-0 flex-1 text-left">
        <span className="block truncate text-sm font-semibold text-foreground">{brand.name}</span>
        {sw && <span className="block truncate text-xs text-muted-foreground">{sw.subheader}</span>}
      </span>
    </>
  );
  if (!sw) return <Link href={BASE} className="flex items-center gap-3 rounded-md p-1 no-underline">{head}</Link>;
  return (
    <DropdownMenu>
      <DropdownMenuTrigger className="flex w-full items-center gap-3 rounded-md border-0 bg-transparent p-1 text-left hover:bg-accent" aria-label={`${brand.name}: switch service`}>
        {head}
        <ChevronsUpDown className="size-4 shrink-0 text-muted-foreground" aria-hidden />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className="w-64">
        {sw.services.map((s) => {
          const here = s.key === sw.current;
          const row = (
            <>
              <NavIcon name={s.icon} className="size-4 shrink-0" style={s.color && !here ? { color: s.color } : undefined} />
              <span className="truncate">{`${s.title} · ${s.key}`}</span>
            </>
          );
          return here || !s.url ? (
            <DropdownMenuItem key={s.key} disabled aria-current="true" className="font-semibold opacity-100 data-[disabled]:opacity-100" style={{ pointerEvents: "none" }} tabIndex={-1}>{row}</DropdownMenuItem>
          ) : (
            <DropdownMenuItem key={s.key} asChild>
              <a href={s.url} className="text-foreground no-underline">{row}</a>
            </DropdownMenuItem>
          );
        })}
        {sw.rows.length > 0 && (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuLabel className="sr-only">Build</DropdownMenuLabel>
            {sw.rows.map((r) => <p key={r} className="m-0 px-2 py-1 font-mono text-xs text-muted-foreground">{r}</p>)}
          </>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

const CHIP = { info: "border-info text-info bg-info-soft", warning: "border-warning text-warning bg-warning-soft", danger: "border-danger text-danger bg-danger-soft" };

/** The environment label in the colour of its level. */
export function EnvChip({ brand }: { brand: api.Brand }) {
  if (!brand.environment) return null;
  const level = envLevel(brand.environment, brand.environment_level);
  return <span data-level={level} className={cn("rounded-full border px-2 py-0.5 text-xs font-semibold tracking-wide uppercase", CHIP[level])}>{brand.environment}</span>;
}

/** Ctrl+K or ⌘K opens a search of the resources by title and name; Enter opens the first match. */
export function CommandSearch({ site, open, onOpenChange }: { site: api.Site; open: boolean; onOpenChange: (open: boolean) => void }) {
  const [text, setText] = useState("");
  const [at, setAt] = useState(0);
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        onOpenChange(true);
      }
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [onOpenChange]);
  const hits = useMemo(() => {
    const needle = text.trim().toLowerCase();
    return site.resources.filter((r) => !needle || r.title.toLowerCase().includes(needle) || r.name.toLowerCase().includes(needle));
  }, [site, text]);
  useEffect(() => setAt(0), [text]);
  const go = (name: string) => {
    onOpenChange(false);
    setText("");
    navigate(listUrl(name));
  };
  return (
    <Dialog open={open} onOpenChange={(o) => { if (!o) setText(""); onOpenChange(o); }}>
      <DialogContent aria-label="Search" closeButton={false} className="m-auto mt-[15vh]">
        <DialogTitle className="sr-only">Search the resources</DialogTitle>
        <div className="relative">
          <Search className="pointer-events-none absolute top-2.5 left-2.5 size-4 text-muted-foreground" aria-hidden />
          <input
            autoFocus
            role="combobox"
            aria-expanded
            aria-controls="command-hits"
            aria-activedescendant={hits[at] ? `command-hit-${hits[at].name}` : undefined}
            aria-label="Search the resources"
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "ArrowDown") { e.preventDefault(); setAt((i) => Math.min(i + 1, hits.length - 1)); }
              else if (e.key === "ArrowUp") { e.preventDefault(); setAt((i) => Math.max(i - 1, 0)); }
              else if (e.key === "Enter" && hits[at]) { e.preventDefault(); go(hits[at].name); }
            }}
            placeholder="Search apps and models…"
            className="h-9 w-full rounded-md border border-input bg-transparent pl-8 text-sm"
          />
        </div>
        <ul id="command-hits" role="listbox" aria-label="Resources" className="m-0 grid max-h-72 list-none gap-0.5 overflow-auto p-0">
          {hits.map((r, i) => (
            // The input keeps the focus and drives the keys, so an option holds no button.
            <li key={r.name} id={`command-hit-${r.name}`} role="option" aria-selected={i === at} onClick={() => go(r.name)} onMouseMove={() => setAt(i)} className={cn("flex w-full cursor-pointer items-center gap-2 rounded-md px-2 py-1.5 text-left text-sm text-foreground", i === at && "bg-accent")}>
              <NavIcon name={r.icon} className="size-4 shrink-0 text-muted-foreground" />
              <span className="flex-1 truncate">{r.title}</span>
              {r.group && <span className="text-xs text-muted-foreground">{r.group}</span>}
            </li>
          ))}
          {hits.length === 0 && <li className="px-2 py-1.5 text-sm text-muted-foreground">No resource matches.</li>}
        </ul>
      </DialogContent>
    </Dialog>
  );
}
