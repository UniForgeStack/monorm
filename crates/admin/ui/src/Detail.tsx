import { useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import * as api from "./api";
import { cell, type Zone } from "./format";
import { ListView, useListState } from "./List";
import { Link, detailUrl, editUrl, listBack, navigate, newUrl } from "./router";
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
  const target = tab && site?.resources.find((r) => r.name === tab.target);
  return (
    <section>
      <p><Link href={listBack(resource)}>← {meta?.title ?? resource}</Link></p>
      <h1>{meta?.title ?? resource} <span className="muted">{decodeURIComponent(d.key)}</span></h1>
      {meta?.writable === false && <p className="muted" role="note">Read-only here: {meta.read_only_reason}</p>}
      {!tab && meta && <RowActions meta={meta} id={id} flags={d.flags ?? {}} detail={d} />}
      {!tab && meta?.bypass && meta.bypass.length > 0 && <div className="toolbar"><Bypass names={meta.bypass} params={params} change={change} /></div>}
      {tabs.length > 0 && <Tabs tabs={tabs} current={tab?.name ?? null} base={detailUrl(resource, id)} />}
      {tab && target && api.may(target, "create") && <AddOnTab tab={tab} detail={d} />}
      {tab && target ? (
        // One list per tab: the key resets the state of the list when the tab changes.
        <ListView key={tab.name} resource={tab.target} zone={zone} meta={target} params={params} change={change} scope={{ resource, key: id, rel: tab.name }} />
      ) : (
        <dl className="fields">
          {d.fields.map((f, i) => (
            <div key={f.label}>
              <dt>{f.title}</dt>
              <dd>{cell(d.values[i], f, zone) || <span className="muted">{f.empty ?? "—"}</span>}</dd>
            </div>
          ))}
          {d.links.map((l) => (
            <div key={l.name}>
              <dt>{l.title}</dt>
              <dd><Link href={detailUrl(l.resource, l.key)}>{l.resource} {decodeURIComponent(l.key)}</Link></dd>
            </div>
          ))}
        </dl>
      )}
    </section>
  );
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
      <Link className="btn btn-primary" href={newUrl(tab.target, text ? `?${text}` : "")}>Add</Link>
    </div>
  );
}

/** Edit and Delete. A `can` rule that the record does not allow turns its button off. Delete asks first. */
function RowActions({ meta, id, flags, detail }: { meta: api.Resource; id: string; flags: api.Flags; detail: api.Detail }) {
  const client = useQueryClient();
  const [asking, setAsking] = useState(false);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string[]>([]);
  if (meta.writable === false) return null;
  const allows = (rule: string) => !(meta.can ?? []).includes(rule) || flags[rule] === true;
  const remove = async () => {
    const loaded: api.Values = {};
    for (const f of meta.form ?? []) {
      const at = detail.fields.findIndex((c) => c.label === f.name);
      if (at >= 0) loaded[f.name] = detail.values[at];
    }
    setBusy(true);
    setProblem([]);
    try {
      await api.destroy(meta.name, id, loaded);
      await client.invalidateQueries({ queryKey: ["list"] });
      await client.invalidateQueries({ queryKey: ["count"] });
      client.removeQueries({ queryKey: ["detail", meta.name, id] });
      navigate(listBack(meta.name));
    } catch (e) {
      const body = e instanceof api.ApiError ? e.body : {};
      const stale = Object.keys(body.conflicts ?? {}).length > 0;
      setProblem(stale ? ["This record changed after you opened it. Reload the page and check it before you delete it."] : body.form?.length ? body.form : [message(e)]);
      setBusy(false);
    }
  };
  return (
    <>
      <div className="actions" role="group" aria-label="Actions">
        {api.may(meta, "update") &&
          (allows("change") ? (
            <Link className="btn btn-plain" href={editUrl(meta.name, id)}>Edit</Link>
          ) : (
            <Button variant="plain" disabled title="This record does not allow change">Edit</Button>
          ))}
        {api.may(meta, "delete") && <Button variant="plain" disabled={!allows("delete") || asking} title={allows("delete") ? undefined : "This record does not allow delete"} onClick={() => setAsking(true)}>Delete</Button>}
      </div>
      {asking && (
        <div className="confirm" role="alertdialog" aria-label="Confirm delete">
          <p>Delete this {meta.title} for good? This cannot be undone.</p>
          {problem.map((m) => <Alert key={m}>{m}</Alert>)}
          <Button disabled={busy} onClick={remove}>Delete it</Button>{" "}
          <Button variant="plain" disabled={busy} onClick={() => { setAsking(false); setProblem([]); }}>Cancel</Button>
        </div>
      )}
    </>
  );
}

/** "Details" and one tab per to-many relation. The tab is `?tab=<relation>` on the detail URL. */
function Tabs({ tabs, current, base }: { tabs: api.Tab[]; current: string | null; base: string }) {
  const items = [{ name: null as string | null, title: "Details" }, ...tabs];
  return (
    <div role="tablist" className="tabs">
      {items.map((t) => (
        <button
          key={t.name ?? ""}
          type="button"
          role="tab"
          aria-selected={t.name === current}
          className={t.name === current ? "tab tab-on" : "tab"}
          onClick={() => navigate(t.name ? `${base}?tab=${encodeURIComponent(t.name)}` : base)}
        >
          {t.title}
        </button>
      ))}
    </div>
  );
}
