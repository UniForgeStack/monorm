import * as React from "react";
import { useEffect, useState, type FormEvent } from "react";
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
function toInput(value: unknown, w: string, zone: Zone): Input {
  if (w === "checkbox") return value === true;
  if (value === null || value === undefined) return "";
  if (w === "json" || w === "list") return JSON.stringify(value);
  if (w === "datetime" && typeof value === "string") {
    const d = new Date(/(Z|[+-]\d\d:?\d\d)$/i.test(value) ? value : `${value}Z`);
    if (Number.isNaN(d.getTime())) return value;
    if (zone === "utc") return d.toISOString().slice(0, 19);
    return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  }
  return String(value);
}

const STRINGS = ["text", "textarea", "select", "radio", "email", "url"];

/** The value to send for the text of an input, or a message when the text cannot be a value of the field. */
function fromInput(input: Input, w: string, nullable: boolean, zone: Zone): { value: unknown } | { error: string } {
  if (typeof input === "boolean") return { value: input };
  const text = input.trim();
  if (STRINGS.includes(w)) {
    if (text === "" && nullable) return { value: null };
    return { value: w === "text" || w === "textarea" ? input : text };
  }
  if (text === "") return { value: nullable ? null : "" };
  if (w === "json" || w === "list") {
    try {
      return { value: JSON.parse(text) };
    } catch {
      return { error: w === "list" ? "Must be a JSON list, like [1, 2]" : "Must be valid JSON" };
    }
  }
  if (w === "datetime") return { value: zone === "local" ? new Date(text).toISOString() : text };
  return { value: text };
}

const inputType = (w: string) => (w === "datetime" ? "datetime-local" : w === "date" || w === "email" || w === "url" ? w : "text");

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

/** What a custom form component gets. `save` sends the values that it gives, and resolves to false when the server refuses them. */
export interface FormProps {
  resource: string;
  creating: boolean;
  /** The value of each form field as the server sent it, or `null` for a new record. */
  values: Record<string, unknown>;
  errors: Problems;
  /** The current value of each field that changed since the form loaded (a 409). */
  conflicts: Record<string, unknown>;
  saving: boolean;
  save: (set: api.Values) => Promise<boolean>;
  reload: () => void;
}

/** The component of a resource that replaces the form: the module default-exports `(React) => Component`. */
function useCustom(meta: api.Resource) {
  return useQuery({
    queryKey: ["form-module", meta.name],
    enabled: meta.form_component === true,
    retry: false,
    staleTime: Infinity,
    gcTime: Infinity,
    queryFn: async () => {
      const module = await import(/* @vite-ignore */ api.formModuleUrl(meta.name));
      return { Component: (module.default as (r: typeof React) => React.ComponentType<FormProps>)(React) };
    },
  });
}

function Block({ section, fields, children }: { section: api.Section; fields: string[]; children: React.ReactNode }) {
  const [open, setOpen] = useState(false);
  // A field error inside a collapsed section opens it.
  const bad = fields.length > 0;
  useEffect(() => {
    if (bad) setOpen(true);
  }, [bad]);
  if (section.title === null) return <div className="section">{children}</div>;
  if (section.collapsed) {
    return (
      <details className="section" open={open} onToggle={(e) => setOpen(e.currentTarget.open)}>
        <summary>{section.title}</summary>
        {children}
      </details>
    );
  }
  return (
    <fieldset className="section">
      <legend>{section.title}</legend>
      {children}
    </fieldset>
  );
}

function FormBody({ meta, columns, cells, id, zone, reload }: { meta: api.Resource; columns: api.Column[]; cells: Record<string, unknown>; id?: string; zone: Zone; reload: () => void }) {
  const client = useQueryClient();
  const creating = id === undefined;
  const fields = (meta.form ?? []).flatMap((f) => {
    const column = columns.find((c) => c.label === f.name);
    return column ? [{ f, column, w: f.widget ?? column.widget }] : [];
  });
  const params = new URLSearchParams(useSearch());
  const blank = (c: api.Column, w: string): Input => toInput(creating ? null : cells[c.label], w, zone);
  const start = () =>
    Object.fromEntries(fields.map(({ column: c, w }) => [c.label, creating && params.has(c.label) ? (w === "checkbox" ? params.get(c.label) === "true" : (params.get(c.label) as string)) : blank(c, w)]));
  const [vals, setVals] = useState<Record<string, Input>>(start);
  const [problems, setProblems] = useState<Problems>({ fields: {}, form: [] });
  const [conflicts, setConflicts] = useState<NonNullable<api.ErrorBody["conflicts"]>>({});
  const [busy, setBusy] = useState(false);
  const custom = useCustom(meta);
  const dirty = (c: api.Column, w: string) => vals[c.label] !== blank(c, w);

  /** The one write of both paths. The guard values go along even for a field that the form cannot change. */
  const send = async (set: api.Values, action: Action): Promise<boolean> => {
    const loaded: api.Values = {};
    if (!creating) for (const { column: c } of fields) loaded[c.label] = cells[c.label];
    setConflicts({});
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
      return true;
    } catch (e) {
      if (!(e instanceof api.ApiError)) setProblems({ fields: {}, form: [message(e)] });
      else {
        if (e.status === 409) setConflicts(e.body.conflicts ?? {});
        setProblems({ fields: e.body.fields ?? {}, form: e.body.form?.length ? e.body.form : e.status === 409 ? [] : [e.message] });
      }
      return false;
    } finally {
      setBusy(false);
    }
  };

  const submit = async (action: Action) => {
    const set: api.Values = {};
    const local: Record<string, string[]> = {};
    for (const { f, column: c, w } of fields) {
      // A read-only field is never sent: the server would refuse it.
      if (f.readonly || (!creating && f.readonly_on_update)) continue;
      // A checkbox that nothing fills and no default covers goes along even when it is off.
      const needed = creating && w === "checkbox" && f.required && !f.default;
      if (!dirty(c, w) && !needed) continue;
      const out = fromInput(vals[c.label] ?? "", w, c.nullable, zone);
      if ("error" in out) local[c.label] = [out.error];
      else set[c.label] = out.value;
    }
    setConflicts({});
    if (Object.keys(local).length > 0) return setProblems({ fields: local, form: [] });
    await send(set, action);
  };
  const on = (action: Action) => (e: FormEvent) => {
    e.preventDefault();
    void submit(action);
  };
  const set = (name: string, v: Input) => setVals((s) => ({ ...s, [name]: v }));

  if (meta.form_component) {
    if (custom.isError) return <Alert>The form of this resource did not load: {message(custom.error)}</Alert>;
    if (!custom.data) return <p className="muted">Loading…</p>;
    const Custom = custom.data.Component;
    const values = Object.fromEntries(fields.map(({ column: c }) => [c.label, creating && params.has(c.label) ? params.get(c.label) : cells[c.label]]));
    return (
      <Custom
        resource={meta.name}
        creating={creating}
        values={values}
        errors={problems}
        conflicts={Object.fromEntries(Object.entries(conflicts).map(([k, v]) => [k, v.current]))}
        saving={busy}
        save={(changes) => send(changes, "save")}
        reload={reload}
      />
    );
  }

  const stale = Object.keys(conflicts).length > 0;
  const unplaced = Object.keys(problems.fields).filter((n) => !fields.some(({ column }) => column.label === n));
  const layout = (creating ? meta.sections?.create : meta.sections?.edit) ?? [{ title: null, fields: fields.map(({ column }) => column.label), collapsed: false }];

  const input = ({ f, column: c, w }: (typeof fields)[number]) => {
    const name = c.label;
    const locked = f.readonly || (!creating && f.readonly_on_update);
    const errors = problems.fields[name] ?? [];
    const conflict = conflicts[name];
    const id_ = `f-${name}`;
    const common = { id: id_, name, disabled: locked, "aria-invalid": errors.length > 0 || undefined, "aria-describedby": `${id_}-msg` };
    const value = vals[name];
    return (
      <div key={name} className={errors.length > 0 || conflict ? "field field-bad" : "field"}>
        <Label.Root id={`${id_}-label`} htmlFor={id_}>
          {c.title}
          {f.required && creating && !f.readonly && <span className="req" title="Required"> *</span>}
          {f.default && creating && <span className="muted"> (default)</span>}
          {f.readonly && <span className="muted"> (read-only)</span>}
        </Label.Root>
        {w === "checkbox" ? (
          <input {...common} type="checkbox" checked={value === true} onChange={(e) => set(name, e.target.checked)} />
        ) : w === "radio" ? (
          <div role="radiogroup" aria-labelledby={`${id_}-label`} aria-invalid={common["aria-invalid"]} aria-describedby={common["aria-describedby"]} className="radios">
            {(c.nullable || creating ? ["", ...(c.choices ?? [])] : (c.choices ?? [])).map((v) => (
              <label key={v} className="radio">
                <input type="radio" name={name} disabled={locked} value={v} checked={value === v} onChange={() => set(name, v)} /> {v === "" ? "—" : v}
              </label>
            ))}
          </div>
        ) : w === "select" ? (
          <select {...common} value={String(value)} onChange={(e) => set(name, e.target.value)}>
            <option value="">{c.nullable || creating ? "—" : "Choose…"}</option>
            {c.choices?.map((v) => <option key={v} value={v}>{v}</option>)}
          </select>
        ) : w === "textarea" || w === "json" || w === "list" ? (
          <textarea {...common} rows={w === "textarea" ? 4 : 6} value={String(value)} maxLength={f.max_length ?? undefined} onChange={(e) => set(name, e.target.value)} spellCheck={w === "textarea"} />
        ) : (
          <input {...common} type={inputType(w)} step={w === "datetime" ? 1 : undefined} inputMode={w === "number" ? "decimal" : undefined} value={String(value)} maxLength={f.max_length ?? undefined} onChange={(e) => set(name, e.target.value)} />
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
  };

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
      {layout.map((section, i) => {
        const inside = section.fields.flatMap((n) => fields.filter(({ column }) => column.label === n));
        if (inside.length === 0) return null;
        const bad = inside.map(({ column }) => column.label).filter((n) => (problems.fields[n]?.length ?? 0) > 0 || conflicts[n]);
        return <Block key={`${section.title ?? ""}:${i}`} section={section} fields={bad}>{inside.map(input)}</Block>;
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
