// The relation fields of a form: a picker for one record, chips for many, and the upload of a file field.
import { useEffect, useId, useLayoutEffect, useRef, useState, type CSSProperties } from "react";
import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { ChevronsUpDown, Plus, X } from "lucide-react";
import * as api from "./api";
import { Button, message } from "./ui";
import { Input } from "./components/ui/input";
import { cn } from "./lib/utils";

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
  const [place, setPlace] = useState<CSSProperties>();
  // The list is `fixed`: a table wrapper with `overflow-x: auto` would clip an absolute one. It sits under the box, or over it when the room below is short.
  useLayoutEffect(() => {
    if (!open) return;
    const at = () => {
      const r = root.current?.getBoundingClientRect();
      if (!r) return;
      const below = window.innerHeight - r.bottom - 8;
      const up = below < 240 && r.top > below;
      setPlace({ left: r.left, width: r.width, maxHeight: Math.min(288, up ? r.top - 8 : below), ...(up ? { bottom: window.innerHeight - r.top + 4 } : { top: r.bottom + 4 }) });
    };
    at();
    window.addEventListener("scroll", at, true);
    window.addEventListener("resize", at);
    return () => {
      window.removeEventListener("scroll", at, true);
      window.removeEventListener("resize", at);
    };
  }, [open]);
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
      className="relative min-w-0 flex-1"
      ref={root}
      onBlur={(e) => {
        if (!root.current?.contains(e.relatedTarget as Node | null)) close();
      }}
    >
      <Input
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
        className="pr-9"
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
      <ChevronsUpDown aria-hidden="true" className="pointer-events-none absolute right-3 top-2.5 size-4 text-muted-foreground" />
      {open && (
        <div style={place} className="fixed z-40 overflow-y-auto rounded-md border border-border bg-popover text-popover-foreground shadow-md">
          <ul id={list} role="listbox" aria-label={`${relation.target} choices`} className="m-0 list-none p-1">
            {items.map((o, i) => (
              <li
                key={JSON.stringify(o.value)}
                id={`${list}-${i}`}
                role="option"
                aria-selected={i === active}
                className={cn("cursor-pointer rounded-sm px-2 py-1.5 text-sm hover:bg-accent", i === active && "bg-accent text-accent-foreground")}
                // The mouse must not take the focus from the input before the click lands.
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => pick(o)}
              >
                {o.label}
              </li>
            ))}
          </ul>
          {found.isError && <p className="m-0 px-2 py-1.5 text-sm text-destructive" role="alert">{message(found.error)}</p>}
          {found.isFetching && <p className="m-0 px-2 py-1.5 text-sm text-muted-foreground" role="status">Loading…</p>}
          {!found.isFetching && !found.isError && items.length === 0 && <p className="m-0 px-2 py-1.5 text-sm text-muted-foreground">No match</p>}
          {(found.hasNextPage || onCreate) && (
            <div className="flex flex-wrap gap-1 border-t border-border p-1">
              {found.hasNextPage && <Button type="button" variant="ghost" size="sm" disabled={found.isFetchingNextPage} onMouseDown={(e) => e.preventDefault()} onClick={() => void found.fetchNextPage()}>Show more</Button>}
              {onCreate && (
                <Button type="button" variant="ghost" size="sm" onMouseDown={(e) => e.preventDefault()} onClick={() => void make()}>
                  <Plus aria-hidden="true" />
                  New {relation.target}…
                </Button>
              )}
            </div>
          )}
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
    <div className="flex items-center gap-2">
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
      {nullable && value !== "" && !disabled && (
        <Button type="button" variant="ghost" size="sm" aria-label={`Clear ${relation.name}`} onClick={() => onChange("")}>
          <X aria-hidden="true" />
          Clear
        </Button>
      )}
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
    <div className="grid gap-2">
      <ul className="m-0 flex list-none flex-wrap gap-1.5 p-0" aria-label={`${relation.name} chips`}>
        {rows.map((o) => (
          <li key={JSON.stringify(o.value)} className="inline-flex items-center gap-1 rounded-full border border-border bg-secondary py-0.5 pl-3 pr-1 text-sm">
            {o.label}
            {!disabled && (
              <button type="button" aria-label={`Remove ${o.label}`} className="grid size-5 place-items-center rounded-full text-muted-foreground hover:bg-accent hover:text-foreground" onClick={() => onChange(rows.filter((r) => !same(r.value, o.value)))}>
                <X aria-hidden="true" className="size-3.5" />
              </button>
            )}
          </li>
        ))}
        {items !== undefined && rows.length === 0 && <li className="text-sm text-muted-foreground">None</li>}
      </ul>
      {items === undefined ? (
        <p className="m-0 text-sm text-muted-foreground">Loading…</p>
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
  if (recordKey === undefined) return <p className="m-0 text-sm text-muted-foreground">Save the record first, then upload the file.</p>;
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
    <div className="grid gap-2">
      <p className="m-0 text-sm">
        {ref ? (
          <>
            <a href={api.fileUrl(resource, recordKey, field)} download>{ref.filename ?? "Download"}</a>
            {typeof ref.size === "number" && <span className="text-muted-foreground"> ({ref.size} bytes)</span>}
          </>
        ) : (
          <span className="text-muted-foreground">No file</span>
        )}
      </p>
      <input id={id} ref={input} type="file" className="block w-full text-sm file:mr-3 file:rounded-md file:border file:border-input file:bg-card file:px-3 file:py-1.5 file:text-sm file:font-medium" disabled={disabled || busy} aria-describedby={described} onChange={(e) => e.target.files?.[0] && void send(e.target.files[0])} />
      {busy && <p className="m-0 text-sm text-muted-foreground" role="status">Uploading…</p>}
      {problem && <p className="m-0 text-sm text-destructive" role="alert">{problem}</p>}
    </div>
  );
}
