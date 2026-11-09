// The relation fields of a form: a picker for one record, chips for many, and the upload of a file field.
import { useEffect, useId, useRef, useState } from "react";
import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import * as api from "./api";
import { message } from "./ui";

/** The value after `ms` of quiet: the picker asks the server for the text that the user stopped typing. */
function useDebounced<T>(value: T, ms: number): T {
  const [out, setOut] = useState(value);
  useEffect(() => {
    const t = setTimeout(() => setOut(value), ms);
    return () => clearTimeout(t);
  }, [value, ms]);
  return out;
}

const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);

/** A search box over the target of a relation. The server pages it by keyset, so a target with millions of rows stays fast. */
function Combobox({ id, resource, relation, text, disabled, invalid, described, exclude = [], onPick, onCreate }: {
  id: string;
  resource: string;
  relation: api.RelationInfo;
  /** What the box shows while it is closed: the name of the chosen record. */
  text: string;
  disabled?: boolean;
  invalid?: boolean;
  described?: string;
  /** Records that the list leaves out: the chips that the form already holds. */
  exclude?: unknown[];
  onPick: (option: api.Option) => void;
  onCreate?: () => Promise<api.Option | null>;
}) {
  const list = useId();
  const root = useRef<HTMLDivElement>(null);
  const [open, setOpen] = useState(false);
  const [typed, setTyped] = useState("");
  const [active, setActive] = useState(0);
  const q = useDebounced(typed, 250);
  const found = useInfiniteQuery({
    queryKey: ["autocomplete", resource, relation.name, q],
    enabled: open,
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) => api.autocomplete(resource, relation.name, q, pageParam),
    getNextPageParam: (p) => p.next ?? undefined,
    staleTime: 10_000,
    retry: false,
  });
  const items = (found.data?.pages ?? []).flatMap((p) => p.items).filter((o) => !exclude.some((e) => same(e, o.value)));
  const close = () => {
    setOpen(false);
    setTyped("");
    setActive(0);
  };
  const pick = (o: api.Option) => {
    onPick(o);
    close();
  };
  const make = async () => {
    close();
    const o = await onCreate?.();
    if (o) onPick(o);
  };
  return (
    <div
      className="combo"
      ref={root}
      onBlur={(e) => {
        if (!root.current?.contains(e.relatedTarget as Node | null)) close();
      }}
    >
      <input
        id={id}
        role="combobox"
        aria-expanded={open}
        aria-controls={list}
        aria-autocomplete="list"
        aria-activedescendant={open && items[active] ? `${list}-${active}` : undefined}
        aria-invalid={invalid || undefined}
        aria-describedby={described}
        autoComplete="off"
        disabled={disabled}
        value={open ? typed : text}
        placeholder={open ? "Search…" : undefined}
        onFocus={() => setOpen(true)}
        onClick={() => setOpen(true)}
        onChange={(e) => {
          setOpen(true);
          setTyped(e.target.value);
          setActive(0);
        }}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown" || e.key === "ArrowUp") {
            e.preventDefault();
            setOpen(true);
            setActive((a) => Math.max(0, Math.min(items.length - 1, a + (e.key === "ArrowDown" ? 1 : -1))));
          } else if (e.key === "Enter" && open) {
            // Enter chooses a row; it never submits the form.
            e.preventDefault();
            const o = items[active];
            if (o) pick(o);
          } else if (e.key === "Escape" && open) {
            e.stopPropagation();
            close();
          }
        }}
      />
      {open && (
        <div className="combo-pop">
          <ul id={list} role="listbox" aria-label={`${relation.target} choices`} className="combo-list">
            {items.map((o, i) => (
              <li
                key={JSON.stringify(o.value)}
                id={`${list}-${i}`}
                role="option"
                aria-selected={i === active}
                className={i === active ? "combo-opt combo-on" : "combo-opt"}
                // The mouse must not take the focus from the input before the click lands.
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => pick(o)}
              >
                {o.label}
              </li>
            ))}
          </ul>
          {found.isError && <p className="msg msg-error" role="alert">{message(found.error)}</p>}
          {found.isFetching && <p className="muted combo-note" role="status">Loading…</p>}
          {!found.isFetching && !found.isError && items.length === 0 && <p className="muted combo-note">No match</p>}
          <div className="combo-foot">
            {found.hasNextPage && <button type="button" className="linkish" disabled={found.isFetchingNextPage} onMouseDown={(e) => e.preventDefault()} onClick={() => void found.fetchNextPage()}>Show more</button>}
            {onCreate && <button type="button" className="linkish" onMouseDown={(e) => e.preventDefault()} onClick={() => void make()}>New {relation.target}…</button>}
          </div>
        </div>
      )}
    </div>
  );
}

/** The name of the record that a key stands for. The target gives it, so the form needs no label of its own. */
function useLabel(relation: api.RelationInfo, target: api.Resource | undefined, value: string) {
  return useQuery({
    queryKey: ["relation-label", relation.target, value],
    enabled: value !== "" && !!target,
    retry: false,
    staleTime: 60_000,
    queryFn: async () => {
      const d = await api.detail(relation.target, encodeURIComponent(value));
      const at = d.fields.findIndex((c) => c.label === relation.label);
      return at < 0 ? value : String(d.values[at] ?? value);
    },
  });
}

/** The picker of a to-one relation field. `value` is the text of the key; `nullable` adds a way to clear it. */
export function Picker({ id, resource, relation, target, value, nullable, disabled, invalid, described, onChange, onCreate }: {
  id: string;
  resource: string;
  relation: api.RelationInfo;
  target: api.Resource | undefined;
  value: string;
  nullable: boolean;
  disabled?: boolean;
  invalid?: boolean;
  described?: string;
  onChange: (value: string) => void;
  onCreate?: () => Promise<api.Option | null>;
}) {
  const [picked, setPicked] = useState<{ value: string; label: string } | null>(null);
  const known = useLabel(relation, target, picked?.value === value ? "" : value);
  const text = value === "" ? "" : picked?.value === value ? picked.label : (known.data ?? (known.isError ? value : ""));
  return (
    <div className="picker">
      <Combobox
        id={id}
        resource={resource}
        relation={relation}
        text={text}
        disabled={disabled}
        invalid={invalid}
        described={described}
        onPick={(o) => {
          setPicked({ value: String(o.value), label: o.label });
          onChange(String(o.value));
        }}
        onCreate={onCreate}
      />
      {nullable && value !== "" && !disabled && <button type="button" className="linkish" aria-label={`Clear ${relation.name}`} onClick={() => onChange("")}>Clear</button>}
    </div>
  );
}

/** The chips of a many-to-many relation: the records now linked, each with a remove button, and a search box that adds one. */
export function Chips({ id, resource, relation, items, disabled, onChange, onCreate }: {
  id: string;
  resource: string;
  relation: api.RelationInfo;
  /** The chips now on the form. `undefined` while the saved links load. */
  items: api.Option[] | undefined;
  disabled?: boolean;
  onChange: (items: api.Option[]) => void;
  onCreate?: () => Promise<api.Option | null>;
}) {
  const rows = items ?? [];
  return (
    <div className="chip-field">
      <ul className="chips" aria-label={`${relation.name} chips`}>
        {rows.map((o) => (
          <li key={JSON.stringify(o.value)} className="chip">
            {o.label}
            {!disabled && <button type="button" aria-label={`Remove ${o.label}`} onClick={() => onChange(rows.filter((r) => !same(r.value, o.value)))}>×</button>}
          </li>
        ))}
        {items !== undefined && rows.length === 0 && <li className="muted">None</li>}
      </ul>
      {items === undefined ? (
        <p className="muted">Loading…</p>
      ) : (
        <Combobox id={id} resource={resource} relation={relation} text="" disabled={disabled} exclude={rows.map((r) => r.value)} onPick={(o) => onChange([...rows, o])} onCreate={onCreate} />
      )}
    </div>
  );
}

/** A file field: the stored file with a download link, and an upload that stores a new one. A new record has no key yet, so it uploads after the first save. */
export function FileField({ id, resource, recordKey, field, reference, disabled, described, onUploaded }: {
  id: string;
  resource: string;
  recordKey: string | undefined;
  field: string;
  reference: unknown;
  disabled?: boolean;
  described?: string;
  onUploaded: (reference: unknown) => void;
}) {
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState("");
  const input = useRef<HTMLInputElement>(null);
  const ref = reference !== null && typeof reference === "object" ? (reference as { filename?: string; size?: number }) : null;
  if (recordKey === undefined) return <p className="muted">Save the record first, then upload the file.</p>;
  const send = async (file: File) => {
    setBusy(true);
    setProblem("");
    try {
      const done = await api.upload(resource, recordKey, field, file);
      onUploaded(done.value);
    } catch (e) {
      setProblem(message(e));
    } finally {
      setBusy(false);
      if (input.current) input.current.value = "";
    }
  };
  return (
    <div className="file-field">
      <p className="file-now">
        {ref ? (
          <>
            <a href={api.fileUrl(resource, recordKey, field)} download>{ref.filename ?? "Download"}</a>
            {typeof ref.size === "number" && <span className="muted"> ({ref.size} bytes)</span>}
          </>
        ) : (
          <span className="muted">No file</span>
        )}
      </p>
      <input id={id} ref={input} type="file" disabled={disabled || busy} aria-describedby={described} onChange={(e) => e.target.files?.[0] && void send(e.target.files[0])} />
      {busy && <p className="muted" role="status">Uploading…</p>}
      {problem && <p className="msg msg-error" role="alert">{problem}</p>}
    </div>
  );
}
