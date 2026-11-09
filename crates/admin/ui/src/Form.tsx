import { useState, type FormEvent } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import * as Label from "@radix-ui/react-label";
import * as api from "./api";
import { cell, type Zone } from "./format";
import { Link, detailUrl, editUrl, listBack, navigate, newUrl, useSearch } from "./router";
import { Alert, Button, message } from "./ui";

type Input = string | boolean;
type Problems = { fields: Record<string, string[]>; form: string[] };
type Action = "save" | "continue" | "add";

const pad = (n: number) => String(n).padStart(2, "0");

/** The text that the input of a field starts with, from the value that the server sent. */
function toInput(value: unknown, c: api.Column, zone: Zone): Input {
  if (c.widget === "checkbox") return value === true;
  if (value === null || value === undefined) return "";
  if (c.widget === "json" || c.widget === "list") return JSON.stringify(value);
  if (c.widget === "datetime" && typeof value === "string") {
    const d = new Date(/(Z|[+-]\d\d:?\d\d)$/i.test(value) ? value : `${value}Z`);
    if (Number.isNaN(d.getTime())) return value;
    if (zone === "utc") return d.toISOString().slice(0, 19);
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  }
  return String(value);
}

/** The value to send for the text of an input, or a message when the text cannot be a value of the field. */
function fromInput(input: Input, c: api.Column, zone: Zone): { value: unknown } | { error: string } {
  if (typeof input === "boolean") return { value: input };
  const text = input.trim();
  if (c.widget === "textarea" || c.widget === "text" || c.widget === "select") {
    if (text === "" && c.nullable) return { value: null };
    return { value: c.widget === "select" ? text : input };
  }
  if (text === "") return { value: c.nullable ? null : "" };
  if (c.widget === "json" || c.widget === "list") {
    try {
      return { value: JSON.parse(text) };
    } catch {
      return { error: c.widget === "list" ? "Must be a JSON list, like [1, 2]" : "Must be valid JSON" };
    }
  }
  if (c.widget === "datetime") return { value: zone === "local" ? new Date(text).toISOString() : text };
  return { value: text };
}

const inputType = (w: string) => (w === "number" ? "text" : w === "datetime" ? "datetime-local" : w === "date" ? "date" : "text");

/** The form of a record: the create form, or the edit form with the values that the server sent. */
export function FormPage({ resource, id, zone, site }: { resource: string; id?: string; zone: Zone; site: api.Site | undefined }) {
  const search = useSearch();
  const meta = site?.resources.find((r) => r.name === resource);
  // Its own key: a focus or a reconnect must not replace the values that the user typed.
  const q = useQuery({ queryKey: ["form", resource, id], queryFn: () => api.detail(resource, id as string), enabled: id !== undefined, retry: false, gcTime: 0, staleTime: Infinity });
  if (!meta) return <p className="muted">Loading…</p>;
  const back = id === undefined ? listBack(resource) : detailUrl(resource, id);
  const head = (
    <>
      <p><Link href={back}>← {id === undefined ? meta.title : `${meta.title} ${decodeURIComponent(id)}`}</Link></p>
      <h1>{id === undefined ? `Add ${meta.title}` : `Change ${meta.title}`} {id !== undefined && <span className="muted">{decodeURIComponent(id)}</span>}</h1>
    </>
  );
  if (meta.writable === false) return <section>{head}<Alert>{meta.read_only_reason ?? "The admin does not write to this model."}</Alert></section>;
  if (q.isError) return <section>{head}<Alert>{message(q.error)}</Alert></section>;
  if (id !== undefined && !q.data) return <section>{head}<p className="muted">Loading…</p></section>;
  const columns = id === undefined ? meta.fields : (q.data as api.Detail).fields;
  const cells = Object.fromEntries(columns.map((c, i) => [c.label, id === undefined ? null : (q.data as api.Detail).values[i]]));
  // A reload after a conflict gives new values: the key makes the form start again with them.
  return (
    <section>
      {head}
      <FormBody key={`${q.dataUpdatedAt}:${search}`} meta={meta} columns={columns} cells={cells} id={id} zone={zone} reload={() => q.refetch()} />
    </section>
  );
}

function FormBody({ meta, columns, cells, id, zone, reload }: { meta: api.Resource; columns: api.Column[]; cells: Record<string, unknown>; id?: string; zone: Zone; reload: () => void }) {
  const client = useQueryClient();
  const creating = id === undefined;
  const fields = (meta.form ?? []).flatMap((f) => {
    const column = columns.find((c) => c.label === f.name);
    return column ? [{ f, column }] : [];
  });
  const params = new URLSearchParams(useSearch());
  const blank = (c: api.Column): Input => toInput(creating ? null : cells[c.label], c, zone);
  const start = () =>
    Object.fromEntries(fields.map(({ column: c }) => [c.label, creating && params.has(c.label) ? (c.widget === "checkbox" ? params.get(c.label) === "true" : (params.get(c.label) as string)) : blank(c)]));
  const [vals, setVals] = useState<Record<string, Input>>(start);
  const [problems, setProblems] = useState<Problems>({ fields: {}, form: [] });
  const [conflicts, setConflicts] = useState<NonNullable<api.ErrorBody["conflicts"]>>({});
  const [busy, setBusy] = useState(false);
  const dirty = (c: api.Column) => vals[c.label] !== blank(c);

  const submit = async (action: Action) => {
    const set: api.Values = {};
    const loaded: api.Values = {};
    const local: Record<string, string[]> = {};
    for (const { f, column: c } of fields) {
      if (!creating) loaded[c.label] = cells[c.label];
      if (!creating && f.readonly_on_update) continue;
      // A checkbox that nothing fills and no default covers goes along even when it is off.
      const needed = creating && c.widget === "checkbox" && f.required && !f.default;
      if (!dirty(c) && !needed) continue;
      const out = fromInput(vals[c.label] ?? "", c, zone);
      if ("error" in out) local[c.label] = [out.error];
      else set[c.label] = out.value;
    }
    setConflicts({});
    if (Object.keys(local).length > 0) return setProblems({ fields: local, form: [] });
    setProblems({ fields: {}, form: [] });
    setBusy(true);
    try {
      const done = creating ? await api.create(meta.name, set) : await api.update(meta.name, id, set, loaded);
      await client.invalidateQueries({ queryKey: ["list"] });
      await client.invalidateQueries({ queryKey: ["count"] });
      await client.invalidateQueries({ queryKey: ["detail"] });
      if (action === "add") {
        // Another record of the same kind: the form is empty again, and the prefill of the URL stays.
        if (creating) setVals(start());
        else navigate(newUrl(meta.name));
      } else if (action === "continue") {
        if (creating) navigate(editUrl(meta.name, done.key));
        else reload();
      } else navigate(detailUrl(meta.name, done.key));
    } catch (e) {
      if (!(e instanceof api.ApiError)) return setProblems({ fields: {}, form: [message(e)] });
      if (e.status === 409) setConflicts(e.body.conflicts ?? {});
      setProblems({ fields: e.body.fields ?? {}, form: e.body.form?.length ? e.body.form : e.status === 409 ? [] : [e.message] });
    } finally {
      setBusy(false);
    }
  };
  const on = (action: Action) => (e: FormEvent) => {
    e.preventDefault();
    void submit(action);
  };
  const set = (name: string, v: Input) => setVals((s) => ({ ...s, [name]: v }));
  const stale = Object.keys(conflicts).length > 0;
  const unplaced = Object.keys(problems.fields).filter((n) => !fields.some(({ column }) => column.label === n));

  return (
    <form className="form" onSubmit={on("save")} noValidate>
      {stale && (
        <Alert>
          This record changed after you opened it. The current value shows under each field that changed.{" "}
          <button type="button" className="linkish" onClick={reload}>Reload the record</button> (your edits are lost).
        </Alert>
      )}
      {problems.form.map((m) => <Alert key={m}>{m}</Alert>)}
      {unplaced.map((n) => <Alert key={n}>{n}: {(problems.fields[n] ?? []).join(" ")}</Alert>)}
      {fields.map(({ f, column: c }) => {
        const name = c.label;
        const locked = !creating && f.readonly_on_update;
        const errors = problems.fields[name] ?? [];
        const conflict = conflicts[name];
        const id_ = `f-${name}`;
        const common = { id: id_, name, disabled: locked, "aria-invalid": errors.length > 0 || undefined, "aria-describedby": `${id_}-msg` };
        const value = vals[name];
        return (
          <div key={name} className={errors.length > 0 || conflict ? "field field-bad" : "field"}>
            <Label.Root htmlFor={id_}>
              {c.title}
              {f.required && creating && <span className="req" title="Required"> *</span>}
              {f.default && creating && <span className="muted"> (default)</span>}
            </Label.Root>
            {c.widget === "checkbox" ? (
              <input {...common} type="checkbox" checked={value === true} onChange={(e) => set(name, e.target.checked)} />
            ) : c.widget === "select" ? (
              <select {...common} value={String(value)} onChange={(e) => set(name, e.target.value)}>
                <option value="">{c.nullable || creating ? "—" : "Choose…"}</option>
                {c.choices?.map((v) => <option key={v} value={v}>{v}</option>)}
              </select>
            ) : c.widget === "textarea" || c.widget === "json" || c.widget === "list" ? (
              <textarea {...common} rows={c.widget === "textarea" ? 4 : 6} value={String(value)} maxLength={f.max_length ?? undefined} onChange={(e) => set(name, e.target.value)} spellCheck={c.widget === "textarea"} />
            ) : (
              <input {...common} type={inputType(c.widget)} step={c.widget === "datetime" ? 1 : undefined} inputMode={c.widget === "number" ? "decimal" : undefined} value={String(value)} maxLength={f.max_length ?? undefined} onChange={(e) => set(name, e.target.value)} />
            )}
            <div id={`${id_}-msg`}>
              {errors.map((m) => <p key={m} className="msg msg-error">{m}</p>)}
              {conflict && (
                <p className="msg msg-error">
                  Changed to <code>{cell(conflict.current, c, zone) || "(empty)"}</code> since you opened this record.
                </p>
              )}
            </div>
          </div>
        );
      })}
      <div className="savebar">
        <Button type="submit" disabled={busy}>Save</Button>
        <Button type="button" variant="plain" disabled={busy} onClick={on("continue")}>Save and continue</Button>
        <Button type="button" variant="plain" disabled={busy} onClick={on("add")}>Save and add another</Button>
        <Link href={creating ? listBack(meta.name) : detailUrl(meta.name, id)} className="cancel">Cancel</Link>
      </div>
    </form>
  );
}
