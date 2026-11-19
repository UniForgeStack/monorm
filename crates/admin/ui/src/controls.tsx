// The control of one plain field value, for the form and for the rows of an inline: one place that picks the kit control for a widget.
import { JsonEditor } from "./components/json-editor";
import { Input, Select, Textarea } from "./components/ui/input";
import { Switch } from "./components/ui/switch";
import { inputType, stepsSeconds, type Input as Value } from "./format";
import type * as api from "./api";

export interface ControlProps {
  w: string;
  column: api.Column;
  value: Value;
  set: (v: Value) => void;
  /** The first option of a select: the empty choice. */
  blank: string;
  id: string;
  name: string;
  disabled?: boolean;
  invalid?: boolean;
  described?: string;
  label?: string;
  maxLength?: number | null;
  rows?: number;
}

/** A switch, a select, a JSON editor, a text area, or an input (a date, a time and a date with a time use the picker of the browser, in its time zone). */
export function ValueControl({ w, column: c, value, set, blank, id, name, disabled, invalid, described, label, maxLength, rows }: ControlProps) {
  const common = { id, name, disabled, "aria-invalid": invalid || undefined, "aria-describedby": described, "aria-label": label };
  if (w === "checkbox") return <Switch {...common} checked={value === true} onChange={(e) => set(e.target.checked)} />;
  if (w === "select" || w === "radio") {
    return (
      <Select {...common} value={String(value)} onChange={(e) => set(e.target.value)}>
        <option value="">{blank}</option>
        {c.choices?.map((v) => <option key={v} value={v}>{v}</option>)}
      </Select>
    );
  }
  if (w === "json") return <JsonEditor {...common} value={String(value)} onValue={set} rows={rows ?? 10} />;
  if (w === "textarea" || w === "list") return <Textarea {...common} rows={rows ?? 6} value={String(value)} maxLength={maxLength ?? undefined} onChange={(e) => set(e.target.value)} spellCheck={w === "textarea"} />;
  return <Input {...common} type={inputType(w)} step={stepsSeconds(w) ? 1 : undefined} inputMode={w === "number" ? "decimal" : undefined} value={String(value)} maxLength={maxLength ?? undefined} onChange={(e) => set(e.target.value)} />;
}
