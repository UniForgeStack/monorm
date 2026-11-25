import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { PanelLeftClose, PanelLeftOpen, Search } from "lucide-react";
import { DetailPage } from "./Detail";
import { FormPage } from "./Form";
import { Home } from "./Home";
import { ListPage } from "./List";
import { Password } from "./Password";
import { Sidebar } from "./Sidebar";
import { CommandSearch, EnvChip, ServiceSwitcher } from "./ShellParts";
import { UserFooter } from "./UserMenu";
import * as api from "./api";
import { Toaster } from "./components/ui/toast";
import { getZone, setZone, type Zone } from "./format";
import { applyBrand } from "./lib/theme";
import { BASE, LOGIN_URL, Link, type Route } from "./router";
import { Alert, Button, message } from "./ui";

const SIDEBAR = "monorm-admin-sidebar";
const wide = () => window.matchMedia("(min-width: 768px)").matches;
const savedOpen = () => {
  try {
    const v = window.localStorage.getItem(SIDEBAR);
    return v === null ? wide() : v === "open";
  } catch {
    return wide();
  }
};

export function Shell({ user, route }: { user: api.User | null; route: Route }) {
  const client = useQueryClient();
  const [zone, setZoneState] = useState<Zone>(getZone());
  const [open, setOpen] = useState(savedOpen);
  const [searching, setSearching] = useState(false);
  const site = useQuery({ queryKey: ["site"], queryFn: api.site, staleTime: Infinity });
  const brand = site.data?.brand;

  useEffect(() => {
    if (!brand) return;
    const serviceHasColor = !!brand.switcher?.services.some((svc) => svc.key === brand.switcher?.current && svc.color);
    applyBrand(brand.colors, serviceHasColor);
    document.title = brand.name;
  }, [brand]);

  const toggle = () => {
    setOpen((o) => {
      try {
        window.localStorage.setItem(SIDEBAR, o ? "closed" : "open");
      } catch {
        // The choice then holds for this page only.
      }
      return !o;
    });
  };
  const signOut = async () => {
    await api.logout().catch(() => undefined);
    client.clear();
    window.location.assign(LOGIN_URL);
  };
  const current = "resource" in route ? route.resource : null;
  const footer = <UserFooter user={user} zone={zone} onZone={(z) => { setZone(z); setZoneState(z); }} onSignOut={signOut} />;

  const title = pageTitle(route, site.data);

  return (
    <div className="flex min-h-screen">
      {site.isError && (
        // The site list is refused (no permission): the account menu stays reachable.
        <aside id="sidebar" hidden={!open} className="flex w-64 shrink-0 flex-col justify-between border-r border-border bg-card p-2">
          <Link href={BASE} className="rounded-md p-2 text-sm font-semibold text-foreground no-underline hover:bg-accent">Admin</Link>
          {footer}
        </aside>
      )}
      {site.data && brand && (
        <Sidebar
          site={site.data}
          open={open}
          current={current}
          home={route.page === "home"}
          onNavigate={() => { if (!wide()) toggle(); }}
          header={<ServiceSwitcher brand={brand} />}
          footer={footer}
        />
      )}
      <div className="flex min-w-0 flex-1 flex-col">
        <header className="sticky top-0 z-30 flex h-14 items-center gap-3 border-b border-border bg-card px-3">
          <Button variant="ghost" size="icon" onClick={toggle} aria-label={open ? "Hide the sidebar" : "Show the sidebar"} aria-expanded={open} aria-controls="sidebar">
            {open ? <PanelLeftClose /> : <PanelLeftOpen />}
          </Button>
          <p className="m-0 truncate text-lg font-semibold text-foreground" data-testid="page-title">{title}</p>
          {brand && <EnvChip brand={brand} />}
          <nav className="ml-4 hidden items-center gap-4 text-sm sm:flex" aria-label="Links">
            {brand?.links.map((l) => (
              <a key={l.url} href={l.url} className="text-muted-foreground hover:text-foreground">{l.label}</a>
            ))}
          </nav>
          {site.data && (
            <Button variant="plain" size="sm" className="ml-auto text-muted-foreground" onClick={() => setSearching(true)} aria-label="Search the resources (Ctrl+K)">
              <Search aria-hidden />
              <span className="hidden sm:inline">Search</span>
            </Button>
          )}
        </header>
        <main className="min-w-0 flex-1 p-4 md:p-6">
          {site.isError && <Alert>{message(site.error)}</Alert>}
          {route.page === "password" && <Password />}
          {route.page === "home" && <Home site={site.data} zone={zone} />}
          {route.page === "list" && <ListPage resource={route.resource} zone={zone} site={site.data} />}
          {route.page === "new" && <FormPage resource={route.resource} zone={zone} site={site.data} />}
          {route.page === "edit" && <FormPage resource={route.resource} id={route.key} zone={zone} site={site.data} />}
          {route.page === "detail" && <DetailPage resource={route.resource} id={route.key} zone={zone} site={site.data} />}
        </main>
      </div>
      {site.data && <CommandSearch site={site.data} open={searching} onOpenChange={setSearching} />}
      <Toaster />
    </div>
  );
}

/** The title of the top bar: the page of the route. */
function pageTitle(route: Route, site: api.Site | undefined): string {
  if (route.page === "home") return "Home";
  if (route.page === "password") return "Change password";
  if (route.page === "login") return "Sign in";
  const title = site?.resources.find((r) => r.name === route.resource)?.title ?? route.resource;
  return route.page === "new" ? `Add ${title}` : title;
}
