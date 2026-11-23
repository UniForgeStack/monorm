import { useState, type ReactNode } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import * as api from "./api";
import { FrameView } from "./Frame";
import { Display } from "./DisplayValue";
import { shown } from "./display";
import { cell, type Zone } from "./format";
import { ListView, useListState } from "./List";
import { Link, detailUrl, editUrl, listBack, navigate, newUrl } from "./router";
import { InlineRowsView } from "./Inlines";
import { InputFields, inputBody, startInputs, type InputValues } from "./Inputs";
import { actionDisabled, ActionDialog, needsDialog, useQuick, type Target } from "./Actions";
import { PackageTab, widgetCall, widgetStream } from "./Packages";
import { ConfirmDialog } from "./components/confirm-dialog";
import { buttonVariants } from "./components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "./components/ui/card";
import { Tabs, TabsList, TabsTrigger } from "./components/ui/tabs";
import { Alert, Bypass, Button, message } from "./ui";

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
      <p><Link href={listBack(resource)}>← {meta?.title ?? resource}</Link></p>
      <h1>{meta?.title ?? resource} <span className="muted">{decodeURIComponent(d.key)}</span></h1>
      {meta?.writable === false && <p className="muted" role="note">Read-only here: {meta.read_only_reason}</p>}
      {!active && meta && <RowActions meta={meta} id={id} flags={d.flags ?? {}} detail={d} />}
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
        <DetailCards detail={fieldsetDetail(d, fieldset.fields)} sections={undefined} zone={zone} title={fieldset.title} />
      ) : (
        <DetailCards detail={d} sections={meta?.sections?.edit} zone={zone} />
      )}
      {!active && meta && <InlineRowsView meta={meta} rows={d.inlines} zone={zone} />}
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

/** Edit and Delete. A `can` rule that the record does not allow turns its button off. Delete asks first. */
function RowActions({ meta, id, flags, detail }: { meta: api.Resource; id: string; flags: api.Flags; detail: api.Detail }) {
  const client = useQueryClient();
  const [asking, setAsking] = useState(false);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string[]>([]);
  const [ins, setIns] = useState<InputValues>(() => startInputs(meta.inputs));
  const [bad, setBad] = useState<Record<string, string[]>>({});
  const [pending, setPending] = useState<api.ActionInfo | null>(null);
  const quick = useQuick(meta.name, (action) => setPending(action));
  const target: Target = { keys: [id], what: "this record" };
  const writable = meta.writable !== false;
  const detailActions = (meta.actions ?? []).filter((a) => a.placement === "detail" && (writable || (a.name === "set_password" && detail.password_target)));
  if (!writable && detailActions.length === 0) return null;
  const allows = (rule: string) => !(meta.can ?? []).includes(rule) || flags[rule] === true;
  const remove = async () => {
    const loaded: api.Values = {};
    for (const f of meta.form ?? []) {
      const at = detail.fields.findIndex((c) => c.label === f.name);
      if (at >= 0) loaded[f.name] = detail.values[at];
    }
    setBusy(true);
    setProblem([]);
    setBad({});
    try {
      await api.destroy(meta.name, id, loaded, inputBody(meta.inputs, ins));
      await client.invalidateQueries({ queryKey: ["list"] });
      await client.invalidateQueries({ queryKey: ["count"] });
      client.removeQueries({ queryKey: ["detail", meta.name, id] });
      navigate(listBack(meta.name));
    } catch (e) {
      const body = e instanceof api.ApiError ? e.body : {};
      const stale = Object.keys(body.conflicts ?? {}).length > 0;
      setBad(body.fields ?? {});
      setProblem(stale ? ["This record changed after you opened it. Reload the page and check it before you delete it."] : body.form?.length ? body.form : [message(e)]);
      setBusy(false);
    }
  };
  return (
    <>
      <div className="actions" role="group" aria-label="Actions">
        {writable && api.may(meta, "update") &&
          (allows("change") ? (
            <Link className={buttonVariants({ variant: "outline" })} href={editUrl(meta.name, id)}>Edit</Link>
          ) : (
            <Button variant="plain" disabled title="This record does not allow change">Edit</Button>
          ))}
        {detailActions.map((a) => {
          const reason = actionDisabled(a, meta.can ?? [], flags);
          return (
            <Button key={a.name} variant="plain" disabled={reason !== undefined} title={reason} onClick={() => (needsDialog(a) ? setPending(a) : void quick(a, target))}>
              {a.label}
            </Button>
          );
        })}
        {writable && api.may(meta, "delete") && <Button variant="plain" disabled={!allows("delete") || asking} title={allows("delete") ? undefined : "This record does not allow delete"} onClick={() => setAsking(true)}>Delete</Button>}
      </div>
      {pending && <ActionDialog key={pending.name} resource={meta.name} action={pending} target={target} passwordTarget={detail.password_target} onClose={() => setPending(null)} />}
      {asking && (
        <ConfirmDialog
          title="Confirm delete"
          description={`Delete this ${meta.title} for good? This cannot be undone.`}
          confirm="Delete it"
          destructive
          busy={busy}
          onConfirm={remove}
          onClose={() => { setAsking(false); setProblem([]); }}
        >
          <div className="grid gap-3">
            {(meta.inputs?.length ?? 0) > 0 && <InputFields defs={meta.inputs ?? []} vals={ins} errors={bad} onChange={(n, v) => setIns((s) => ({ ...s, [n]: v }))} scope="delete" />}
            {problem.map((m) => <Alert key={m}>{m}</Alert>)}
          </div>
        </ConfirmDialog>
      )}
    </>
  );
}

/** "Details" and one tab per to-many relation. The tab is `?tab=<relation>` on the detail URL. */
function DetailTabs({ tabs, current, base }: { tabs: { name: string; title: string }[]; current: string | null; base: string }) {
  const items = [{ name: DETAILS, title: "Details" }, ...tabs];
  return (
    <Tabs value={current ?? DETAILS} onValueChange={(name) => navigate(name === DETAILS ? base : `${base}?tab=${encodeURIComponent(name)}`)} className="mb-4">
      <TabsList aria-label="Sections" className="flex-wrap">
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

const rowClass = "grid items-baseline gap-1 border-b border-border py-2.5 last:border-b-0 sm:grid-cols-[12rem_1fr] sm:gap-4";

/** The fields of a record as cards: one per section of the resource, or one "Details" card when it has none. What no section names, and the links, go in the last card without a title (or a new "Other" card). */
export function DetailCards({ detail: d, sections, zone, title = "Details" }: { detail: Pick<api.Detail, "fields" | "values" | "links">; sections: api.Section[] | undefined; zone: Zone; title?: string }) {
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
  const cards: { title: string; rows: ReactNode[]; plain: boolean }[] = (sections ?? []).map((s) => ({
    title: s.title ?? (titled ? "Other" : "Details"),
    plain: !s.title,
    rows: s.fields.flatMap((n) => { const i = d.fields.findIndex((f) => f.label === n); return i < 0 ? [] : [row(d.fields[i] as api.Column, i)]; }),
  }));
  const rest = [...d.fields.flatMap((f, i) => (named.has(f.label) ? [] : [row(f, i)])), ...links];
  const last = [...cards].reverse().find((c) => c.plain);
  if (rest.length > 0) {
    if (last) last.rows.push(...rest);
    else cards.push({ title: cards.length === 0 ? title : "Other", plain: true, rows: rest });
  }
  return (
    <>
      {cards.filter((c) => c.rows.length > 0).map((c, i) => (
        <Card key={i} className="mb-4" role="region" aria-labelledby={`detail-card-${i}`}>
          <CardHeader>
            <CardTitle id={`detail-card-${i}`}>{c.title}</CardTitle>
          </CardHeader>
          <CardContent>
            <dl className="m-0">{c.rows}</dl>
          </CardContent>
        </Card>
      ))}
    </>
  );
}
