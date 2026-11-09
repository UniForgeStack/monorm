import { useQuery } from "@tanstack/react-query";
import * as api from "./api";
import { cell, type Zone } from "./format";
import { ListView, useListState } from "./List";
import { Link, detailUrl, listBack, navigate } from "./router";
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
      {!tab && <RowActions names={meta?.can ?? []} flags={d.flags ?? {}} />}
      {!tab && meta?.bypass && meta.bypass.length > 0 && <div className="toolbar"><Bypass names={meta.bypass} params={params} change={change} /></div>}
      {tabs.length > 0 && <Tabs tabs={tabs} current={tab?.name ?? null} base={detailUrl(resource, id)} />}
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

/** One button for each `can` rule of the resource. The button of a rule that the record does not allow is off. */
function RowActions({ names, flags }: { names: string[]; flags: api.Flags }) {
  if (names.length === 0) return null;
  return (
    <div className="actions" role="group" aria-label="Actions">
      {names.map((n) => (
        <Button key={n} variant="plain" disabled={!flags[n]} title={flags[n] ? undefined : `This record does not allow ${n}`}>
          {n.charAt(0).toUpperCase() + n.slice(1)}
        </Button>
      ))}
    </div>
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
