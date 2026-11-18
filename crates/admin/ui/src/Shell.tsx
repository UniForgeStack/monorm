import { useEffect, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { LayoutDashboard, PanelLeftClose, PanelLeftOpen } from "lucide-react";
import { DetailPage } from "./Detail";
import { FormPage } from "./Form";
import { Home } from "./Home";
import { ListPage } from "./List";
import { Password } from "./Password";
import { Sidebar } from "./Sidebar";
import { UserMenu } from "./UserMenu";
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
  const site = useQuery({ queryKey: ["site"], queryFn: api.site, staleTime: Infinity });
  const brand = site.data?.brand;

  useEffect(() => {
    if (!brand) return;
    applyBrand(brand.colors);
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

  return (
    <div className="flex min-h-screen flex-col">
      <header className="sticky top-0 z-30 flex h-14 items-center gap-2 border-b border-border bg-card px-3">
        <Button variant="ghost" size="icon" onClick={toggle} aria-label={open ? "Hide the sidebar" : "Show the sidebar"} aria-expanded={open} aria-controls="sidebar">
          {open ? <PanelLeftClose /> : <PanelLeftOpen />}
        </Button>
        <Link href={BASE} className="flex items-center gap-2 rounded-md px-1 text-lg font-semibold text-foreground no-underline">
          {brand?.logo ? <img src={brand.logo} alt="" className="h-7 w-auto" /> : <span className="grid size-7 place-items-center rounded-md bg-primary text-primary-foreground"><LayoutDashboard className="size-4" aria-hidden /></span>}
          <span>{brand?.name ?? "Admin"}</span>
        </Link>
        {brand?.environment && <span className="rounded-full border border-warning px-2 py-0.5 text-xs font-medium text-warning">{brand.environment}</span>}
        <nav className="ml-4 hidden items-center gap-4 text-sm sm:flex" aria-label="Links">
          {brand?.links.map((l) => (
            <a key={l.url} href={l.url} className="text-muted-foreground hover:text-foreground">{l.label}</a>
          ))}
        </nav>
        <div className="ml-auto">
          <UserMenu user={user} zone={zone} onZone={(z) => { setZone(z); setZoneState(z); }} onSignOut={signOut} />
        </div>
      </header>
      <div className="flex flex-1">
        {site.data && <Sidebar site={site.data} open={open} current={current} home={route.page === "home"} onNavigate={() => { if (!wide()) toggle(); }} />}
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
      <Toaster />
    </div>
  );
}
