import type * as api from "./api";
import { Input, Select, Textarea } from "./components/ui/input";
import { Label } from "./components/ui/label";
import { Switch } from "./components/ui/switch";
import { cn } from "./lib/utils";

export type InputValues = Record<string, string | boolean>;

/** The start values of the controls: a checkbox is off, the others are empty. */
export const startInputs = (defs: api.InputInfo[] | undefined): InputValues => Object.fromEntries((defs ?? []).map((d) => [d.name, d.type === "checkbox" ? false : ""]));

/** The `inputs` of a request, or nothing when the resource has no input. The server checks each value again. */
export const inputBody = (defs: api.InputInfo[] | undefined, vals: InputValues): api.Values | undefined => (defs && defs.length > 0 ? { ...vals } : undefined);

/** One control for each input of the service. The service reads them for this one request only. */
export function InputFields({ defs, vals, errors, onChange, scope }: { defs: api.InputInfo[]; vals: InputValues; errors: Record<string, string[]>; onChange: (name: string, v: string | boolean) => void; scope: string }) {
  return (
    <>
      {defs.map((d) => {
        const id = `in-${scope}-${d.name}`;
        const bad = errors[d.name] ?? [];
        const common = { id, name: d.name, "aria-invalid": bad.length > 0 || undefined, "aria-describedby": `${id}-msg` };
        const value = vals[d.name];
        return (
          <div key={d.name} className={cn("mb-4 grid gap-1.5", bad.length > 0 && "field-bad")}>
            <Label htmlFor={id}>
              {d.label}
              {d.required && d.type !== "checkbox" && <span className="text-destructive" title="Required"> *</span>}
            </Label>
            {d.type === "checkbox" ? (
              <Switch {...common} checked={value === true} onChange={(e) => onChange(d.name, e.target.checked)} />
            ) : d.type === "select" ? (
              <Select {...common} value={String(value)} onChange={(e) => onChange(d.name, e.target.value)}>
                <option value="">—</option>
                {d.choices.map((c) => <option key={c} value={c}>{c}</option>)}
              </Select>
            ) : d.type === "textarea" ? (
              <Textarea {...common} rows={4} value={String(value)} onChange={(e) => onChange(d.name, e.target.value)} />
            ) : (
              <Input {...common} type="text" inputMode={d.type === "number" ? "decimal" : undefined} value={String(value)} onChange={(e) => onChange(d.name, e.target.value)} />
            )}
            <div id={`${id}-msg`}>
              {d.help && <p className="m-0 text-sm text-muted-foreground">{d.help}</p>}
              {bad.map((m) => <p key={m} className="msg msg-error m-0 text-sm text-destructive">{m}</p>)}
            </div>
          </div>
        );
      })}
    </>
  );
}
