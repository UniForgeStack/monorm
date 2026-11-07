import { useEffect, useState, type FormEvent } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import * as Menu from "@radix-ui/react-dropdown-menu";
import { Table } from "./Table";
import * as api from "./api";
import { cell, getZone, setZone, type Zone } from "./format";
import { BASE, LOGIN_URL, Link, PASSWORD_URL, detailUrl, listUrl, navigate, parse, usePath } from "./router";
import { Alert, Button, Card, Field } from "./ui";

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

export function App() {
  const path = usePath();
  const route = parse(path);
  const client = useQueryClient();
  const state = useQuery({ queryKey: ["session"], queryFn: api.session, retry: false, staleTime: Infinity });

  useEffect(() => {
    if (state.data && !state.data.authenticated && route.page !== "login") navigate(LOGIN_URL, true);
    if (state.data?.authenticated && route.page === "login") navigate(BASE, true);
  }, [state.data, route.page]);

  if (state.isPending) return <p className="center muted">Loading…</p>;
  if (state.isError) return <div className="center"><Alert>{message(state.error)}</Alert></div>;
  if (!state.data.authenticated) return <Login onDone={() => client.invalidateQueries({ queryKey: ["session"] })} />;
  return <Shell user={state.data.user} route={route} />;
}

function Login({ onDone }: { onDone: () => void }) {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const submit = async (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    const form = new FormData(e.currentTarget);
    setBusy(true);
    setError(null);
    try {
      await api.login(String(form.get("username")), String(form.get("password")));
      navigate(BASE, true);
      onDone();
    } catch (err) {
      setError(message(err));
    } finally {
      setBusy(false);
    }
  };
  return (
    <main className="center">
      <Card title="Sign in">
        <form onSubmit={submit}>
          <Field label="Username" name="username" autoComplete="username" required autoFocus />
          <Field label="Password" name="password" type="password" autoComplete="current-password" required />
          {error && <Alert>{error}</Alert>}
          <Button type="submit" disabled={busy}>Sign in</Button>
        </form>
      </Card>
    </main>
  );
}

function Shell({ user, route }: { user: api.User | null; route: ReturnType<typeof parse> }) {
  const client = useQueryClient();
  const [zone, setZoneState] = useState<Zone>(getZone());
  const site = useQuery({ queryKey: ["site"], queryFn: api.site, staleTime: Infinity });

  useEffect(() => {
    const brand = site.data?.brand;
    if (!brand) return;
    const root = document.documentElement.style;
    if (brand.colors.primary) root.setProperty("--primary", brand.colors.primary);
    if (brand.colors.accent) root.setProperty("--accent", brand.colors.accent);
    document.title = brand.name;
  }, [site.data]);

  const signOut = async () => {
    await api.logout().catch(() => undefined);
    client.clear();
    navigate(LOGIN_URL);
    window.location.reload();
  };
  const brand = site.data?.brand;
  const byName = new Map(site.data?.resources.map((r) => [r.name, r]));
  const label = user?.display ?? user?.username ?? "Account";

  return (
    <div className="shell">
      <header className="header">
        <Link href={BASE} className="brand">
          {brand?.logo && <img src={brand.logo} alt="" height={24} />}
          <span>{brand?.name ?? "Admin"}</span>
        </Link>
        {brand?.environment && <span className="env">{brand.environment}</span>}
        <nav className="links">
          {brand?.links.map((l) => (
            <a key={l.url} href={l.url}>{l.label}</a>
          ))}
        </nav>
        <Menu.Root>
          <Menu.Trigger className="btn btn-plain" aria-label="User menu">{label}</Menu.Trigger>
          <Menu.Portal>
            <Menu.Content className="menu" align="end" sideOffset={4}>
              <Menu.Label className="menu-label">Time zone</Menu.Label>
              <Menu.RadioGroup value={zone} onValueChange={(v) => { setZone(v as Zone); setZoneState(v as Zone); }}>
                <Menu.RadioItem className="menu-item" value="local"><Menu.ItemIndicator>✓ </Menu.ItemIndicator>Browser time zone</Menu.RadioItem>
                <Menu.RadioItem className="menu-item" value="utc"><Menu.ItemIndicator>✓ </Menu.ItemIndicator>UTC</Menu.RadioItem>
              </Menu.RadioGroup>
              <Menu.Separator className="menu-sep" />
              <Menu.Item className="menu-item" onSelect={() => navigate(PASSWORD_URL)}>Change password</Menu.Item>
              <Menu.Item className="menu-item" onSelect={signOut}>Sign out</Menu.Item>
            </Menu.Content>
          </Menu.Portal>
        </Menu.Root>
      </header>
      <div className="body">
        <nav className="side" aria-label="Resources">
          {site.data?.nav.map((g) => (
            <div key={g.group ?? ""} className="group">
              {g.group && <h2>{g.group}</h2>}
              <ul>
                {g.resources.map((n) => (
                  <li key={n}>
                    <Link href={listUrl(n)} aria-current={"resource" in route && route.resource === n ? "page" : undefined}>
                      {byName.get(n)?.title ?? n}
                    </Link>
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </nav>
        <main className="main">
          {site.isError && <Alert>{message(site.error)}</Alert>}
          {route.page === "password" && <Password />}
          {route.page === "home" && <Card title={brand?.name ?? "Admin"}><p className="muted">Choose a resource.</p></Card>}
          {route.page === "list" && <ListPage resource={route.resource} zone={zone} site={site.data} />}
          {route.page === "detail" && <DetailPage resource={route.resource} id={route.key} zone={zone} site={site.data} />}
        </main>
      </div>
    </div>
  );
}

function Password() {
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);
  const submit = async (e: FormEvent<HTMLFormElement>) => {
    e.preventDefault();
    const form = new FormData(e.currentTarget);
    setError(null);
    setDone(false);
    if (form.get("new") !== form.get("again")) return setError("The two new passwords differ.");
    try {
      await api.changePassword(String(form.get("old")), String(form.get("new")));
      setDone(true);
      e.currentTarget.reset();
    } catch (err) {
      setError(message(err));
    }
  };
  return (
    <Card title="Change password">
      <form onSubmit={submit}>
        <Field label="Current password" name="old" type="password" autoComplete="current-password" required />
        <Field label="New password" name="new" type="password" autoComplete="new-password" required />
        <Field label="New password again" name="again" type="password" autoComplete="new-password" required />
        {error && <Alert>{error}</Alert>}
        {done && <Alert kind="ok">The password changed.</Alert>}
        <Button type="submit">Change password</Button>
      </form>
    </Card>
  );
}

function ListPage({ resource, zone, site }: { resource: string; zone: Zone; site: api.Site | undefined }) {
  const [cursors, setCursors] = useState<(string | null)[]>([null]);
  useEffect(() => setCursors([null]), [resource]);
  const cursor = cursors[cursors.length - 1] ?? null;
  const q = useQuery({ queryKey: ["list", resource, cursor], queryFn: () => api.page(resource, cursor), placeholderData: (prev) => prev });
  const meta = site?.resources.find((r) => r.name === resource);
  if (q.isError) return <Alert>{message(q.error)}</Alert>;
  if (!q.data) return <p className="muted">Loading…</p>;
  const page = q.data;
  return (
    <section>
      <h1>{meta?.title ?? resource}</h1>
      <Table resource={resource} columns={page.columns} rows={page.rows} keys={page.keys} zone={zone} />
      {page.rows.length === 0 && <p className="muted">No records.</p>}
      <div className="pager">
        <Button variant="plain" disabled={cursors.length < 2} onClick={() => setCursors((c) => c.slice(0, -1))}>Previous</Button>
        <Button variant="plain" disabled={!page.next} onClick={() => page.next && setCursors((c) => [...c, page.next])}>Next</Button>
      </div>
    </section>
  );
}

function DetailPage({ resource, id, zone, site }: { resource: string; id: string; zone: Zone; site: api.Site | undefined }) {
  const q = useQuery({ queryKey: ["detail", resource, id], queryFn: () => api.detail(resource, id), retry: false });
  const meta = site?.resources.find((r) => r.name === resource);
  if (q.isError) return <Alert>{message(q.error)}</Alert>;
  if (!q.data) return <p className="muted">Loading…</p>;
  const d = q.data;
  return (
    <section>
      <p><Link href={listUrl(resource)}>← {meta?.title ?? resource}</Link></p>
      <h1>{meta?.title ?? resource} <span className="muted">{decodeURIComponent(d.key)}</span></h1>
      <dl className="fields">
        {d.fields.map((f, i) => (
          <div key={f.label}>
            <dt>{f.title}</dt>
            <dd>{cell(d.values[i], f, zone) || <span className="muted">—</span>}</dd>
          </div>
        ))}
        {d.links.map((l) => (
          <div key={l.name}>
            <dt>{l.title}</dt>
            <dd><Link href={detailUrl(l.resource, l.key)}>{l.resource} {decodeURIComponent(l.key)}</Link></dd>
          </div>
        ))}
      </dl>
    </section>
  );
}
