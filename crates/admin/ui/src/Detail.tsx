import type { ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import * as api from "./api";
import { FrameView } from "./Frame";
import { Display } from "./DisplayValue";
import { shown } from "./display";
import { cell, type Zone } from "./format";
import { ListView, useListState } from "./List";
import { Link, detailUrl, listBack, navigate, newUrl } from "./router";
import { InlineRowsView } from "./Inlines";
import { DetailFooter, DetailHeaderActions, loadedValues } from "./RecordActions";
import { Breadcrumbs } from "./Bars";
import { PackageTab, widgetCall, widgetStream } from "./Packages";
import { buttonVariants } from "./components/ui/button";
import { Card, CardContent } from "./components/ui/card";
import { Section } from "./components/section";
import { Tabs, TabsList, TabsTrigger } from "./components/ui/tabs";
import { Alert, Bypass, message } from "./ui";

/** A record: its fields and links, and one tab per to-many relation whose target the user can read. */
export function DetailPage({ resource, id, zone, site }: { resource: string; id: string; zone: Zone; site: api.Site | undefined }) {
  const { params, change } = useListState();
  const chosen = params.get("tab");
  // On a tab the `without` parameter belongs to the child list: the record itself keeps its read defaults.
  const without = chosen ? "" : (params.get("without") ?? "");
  const q = useQuery({ queryKey: ["detail", resource, id, without], queryFn: () => api.detail(resource, id, without), retry: false });
  const meta = site?.resources.find((r) => r.name === resource);
  if (q.isError) return <Alert>{message(q.error)}</Alert>;
  if (!q.data) return <p className="muted">Loading…</p>;
  const d = q.data;
  const tabs = (meta?.tabs ?? []).filter((t) => site?.resources.some((r) => r.name === t.target));
  const tab = tabs.find((t) => t.name === chosen);
  const packageTabs = meta?.package_tabs ?? [];
  const packageTab = tab ? undefined : packageTabs.find((t) => t.name === chosen);
  const fieldsets = meta?.fieldsets ?? [];
  const fieldset = fieldsets.find((t) => t.name === chosen);
  const frames = d.frames ?? [];
  const frame = frames.find((f) => f.name === chosen);
  const active = tab ?? packageTab ?? fieldset ?? frame;
  const allTabs = [...fieldsets, ...tabs, ...packageTabs, ...frames];
  const target = tab && site?.resources.find((r) => r.name === tab.target);
  return (
    <section>
      <Breadcrumbs group={meta?.group} items={[{ label: meta?.title ?? resource, href: listBack(resource) }, { label: decodeURIComponent(d.key) }]} />
      <div className="mb-4 flex flex-wrap items-center justify-between gap-3">
        <h1 className="m-0">{meta?.title ?? resource} <span className="muted">{decodeURIComponent(d.key)}</span></h1>
        {!active && meta && <DetailHeaderActions meta={meta} id={id} flags={d.flags ?? {}} detail={d} hasAudit={!!site?.resources.some((r) => r.name === "AdminLogEntry")} />}
      </div>
      {meta?.writable === false && <p className="muted" role="note">Read-only here: {meta.read_only_reason}</p>}
      {!active && meta?.bypass && meta.bypass.length > 0 && <div className="toolbar"><Bypass names={meta.bypass} params={params} change={change} /></div>}
      {allTabs.length > 0 && <DetailTabs tabs={allTabs} current={active?.name ?? null} base={detailUrl(resource, id)} />}
      {tab && target && api.may(target, "create") && <AddOnTab tab={tab} detail={d} />}
      {packageTab && site ? (
        <PackageTab key={packageTab.name} site={site} name={packageTab.name} embedded={false} resource={resource} id={id} record={Object.fromEntries(d.fields.map((f, i) => [f.label, d.values[i]]))} call={widgetCall} stream={widgetStream} />
      ) : tab && target ? (
        // One list per tab: the key resets the state of the list when the tab changes.
        <ListView key={tab.name} resource={tab.target} zone={zone} meta={target} params={params} change={change} scope={{ resource, key: id, rel: tab.name }} />
      ) : frame ? (
        <FrameView key={frame.name} frame={frame} />
      ) : fieldset ? (
        <DetailCards resource={resource} detail={fieldsetDetail(d, fieldset.fields)} sections={undefined} zone={zone} title={fieldset.title} />
      ) : (
        <>
          <DetailCards resource={resource} detail={d} sections={meta?.sections?.edit} zone={zone} />
          {(meta?.linked ?? []).length > 0 && <LinkedCard resource={resource} id={id} rels={meta?.linked ?? []} />}
        </>
      )}
      {!active && meta && <InlineRowsView meta={meta} rows={d.inlines} zone={zone} />}
      {!active && meta && <DetailFooter meta={meta} id={id} flags={d.flags ?? {}} loaded={loadedValues(meta, d.fields, d.values)} />}
    </section>
  );
}

function fieldsetDetail(detail: api.Detail, fields: string[]) {
  const indices = fields.map((name) => detail.fields.findIndex((field) => field.label === name)).filter((i) => i >= 0);
  return { fields: indices.map((i) => detail.fields[i]!), values: indices.map((i) => detail.values[i]), links: [] };
}

/** The values of the open record that "Add" on a tab carries to the form of the new child record. */
function AddOnTab({ tab, detail }: { tab: api.Tab; detail: api.Detail }) {
  const params = new URLSearchParams();
  for (const p of tab.prefill ?? []) {
    const at = detail.fields.findIndex((f) => f.label === p.parent);
    const v = at < 0 ? null : detail.values[at];
    if (v !== null && v !== undefined) params.set(p.child, String(v));
  }
  const text = params.toString();
  return (
    <div className="actions">
      <Link className={buttonVariants()} href={newUrl(tab.target, text ? `?${text}` : "")}>Add</Link>
    </div>
  );
}

/** "Details" and one tab per to-many relation. The tab is `?tab=<relation>` on the detail URL. */
function DetailTabs({ tabs, current, base }: { tabs: { name: string; title: string }[]; current: string | null; base: string }) {
  const items = [{ name: DETAILS, title: "Details" }, ...tabs];
  return (
    <Tabs value={current ?? DETAILS} onValueChange={(name) => navigate(name === DETAILS ? base : `${base}?tab=${encodeURIComponent(name)}`)} className="mb-4">
      <TabsList variant="pill" aria-label="Sections" className="flex-wrap">
        {items.map((t) => <TabsTrigger key={t.name} value={t.name}>{t.title}</TabsTrigger>)}
      </TabsList>
    </Tabs>
  );
}
const DETAILS = "details:default";

/** The badge of a column that has one; the detail page draws no bool icon. */
function badged(value: unknown, f: api.Column) {
  const look = shown(value, f, false);
  return look ? <Display shown={look} /> : null;
}

/** The read-only relations of a record: one row each, with a link to every related record. */
function LinkedCard({ resource, id, rels }: { resource: string; id: string; rels: NonNullable<api.Resource["linked"]> }) {
  return (
    <Card className="mb-4" role="region" aria-label="Related records">
      <CardContent>
        <dl className="m-0">{rels.map((r) => <LinkedRow key={r.name} resource={resource} id={id} rel={r} />)}</dl>
      </CardContent>
    </Card>
  );
}

function LinkedRow({ resource, id, rel }: { resource: string; id: string; rel: { name: string; title: string } }) {
  const q = useQuery({ queryKey: ["linked", resource, id, rel.name], queryFn: () => api.linked(resource, id, rel.name), retry: false });
  const items = q.data?.items ?? [];
  return (
    <div className={rowClass}>
      <dt className="text-sm text-muted-foreground">{rel.title}</dt>
      <dd className="m-0 break-words">
        {q.isError ? <span className="text-muted-foreground">—</span> : items.length === 0 ? <span className="text-muted-foreground">{q.data ? "—" : "Loading…"}</span> : (
          <ul className="m-0 list-none p-0">
            {items.map((o) => <li key={`${o.resource}:${o.value}`}><Link href={detailUrl(o.resource, encodeURIComponent(String(o.value)))}>{o.label}</Link></li>)}
            {q.data?.truncated && <li className="text-muted-foreground">…</li>}
          </ul>
        )}
      </dd>
    </div>
  );
}

const rowClass = "grid items-baseline gap-1 border-b border-border py-2.5 last:border-b-0 sm:grid-cols-[12rem_1fr] sm:gap-4";

/** The fields of a record as cards: one per section of the resource, or one "Details" card when it has none. What no section names, and the links, go in the last card without a title (or a new "Other" card). */
export function DetailCards({ resource, detail: d, sections, zone, title = "Details" }: { resource?: string; detail: Pick<api.Detail, "fields" | "values" | "links">; sections: api.Section[] | undefined; zone: Zone; title?: string }) {
  const row = (f: api.Column, i: number) => (
    <div key={f.label} className={rowClass}>
      <dt className="text-sm text-muted-foreground">{f.title}</dt>
      <dd className="m-0 break-words">{f.widget === "json" && d.values[i] !== null && typeof d.values[i] === "object" ? <pre className="json-value rounded-md bg-muted p-3">{JSON.stringify(d.values[i], null, 2)}</pre> : (badged(d.values[i], f) ?? (cell(d.values[i], f, zone) || <span className="text-muted-foreground">{f.empty ?? "—"}</span>))}</dd>
    </div>
  );
  const links = d.links.map((l) => (
    <div key={`link:${l.name}`} className={rowClass}>
      <dt className="text-sm text-muted-foreground">{l.title}</dt>
      <dd className="m-0 break-words"><Link href={detailUrl(l.resource, l.key)}>{l.resource} {decodeURIComponent(l.key)}</Link></dd>
    </div>
  ));
  const named = new Set((sections ?? []).flatMap((s) => s.fields));
  const titled = (sections ?? []).some((s) => s.title);
  const cards: { title: string; rows: ReactNode[]; plain: boolean; collapsed: boolean }[] = (sections ?? []).map((s) => ({
    title: s.title ?? (titled ? "Other" : "Details"),
    plain: !s.title,
    collapsed: !!s.title && s.collapsed,
    rows: s.fields.flatMap((n) => { const i = d.fields.findIndex((f) => f.label === n); return i < 0 ? [] : [row(d.fields[i] as api.Column, i)]; }),
  }));
  const rest = [...d.fields.flatMap((f, i) => (named.has(f.label) ? [] : [row(f, i)])), ...links];
  const last = [...cards].reverse().find((c) => c.plain);
  if (rest.length > 0) {
    if (last) last.rows.push(...rest);
    else cards.push({ title: cards.length === 0 ? title : "Other", plain: true, collapsed: false, rows: rest });
  }
  const seen = new Map<string, number>();
  const shown = cards.filter((c) => c.rows.length > 0).map((c) => {
    const n = (seen.get(c.title) ?? 0) + 1;
    seen.set(c.title, n);
    return { ...c, name: n === 1 ? c.title : `${c.title}#${n}` };
  });
  return (
    <>
      {shown.map((c, i) => (
        <Section key={c.name} resource={resource} name={c.name} title={c.title} collapsed={c.collapsed} id={`detail-card-${i}`}>
          <dl className="m-0">{c.rows}</dl>
        </Section>
      ))}
    </>
  );
}
