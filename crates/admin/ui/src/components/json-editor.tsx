import { useId, useState, type TextareaHTMLAttributes } from "react";
import { Braces, Check } from "lucide-react";
import { cn } from "../lib/utils";
import { Button } from "./ui/button";
import { Textarea } from "./ui/input";
import { parseJson } from "../format";

/** A monospace box for JSON, with Format (two-space indent) and Validate. The text stays the form value: the server and the save check it again. */
export function JsonEditor({ value, onValue, className, ...props }: { value: string; onValue: (text: string) => void } & Omit<TextareaHTMLAttributes<HTMLTextAreaElement>, "value" | "onChange">) {
  const status = useId();
  const [checked, setChecked] = useState(false);
  const parsed = value.trim() === "" ? null : parseJson(value);
  const problem = parsed && "error" in parsed ? parsed.error : null;
  const locked = props.disabled || props.readOnly;
  const format = () => {
    setChecked(true);
    if (parsed && "value" in parsed) onValue(JSON.stringify(parsed.value, null, 2));
  };
  return (
    <div className="grid gap-2">
      <Textarea {...props} value={value} onChange={(e) => (setChecked(false), onValue(e.target.value))} spellCheck={false} aria-describedby={cn(props["aria-describedby"], status)} className={cn("min-h-48 font-mono text-[0.85rem] leading-relaxed", className)} />
      <div className="flex flex-wrap items-center gap-2">
        <Button type="button" variant="outline" size="sm" disabled={locked} onClick={format} title="Format rewrites the text. A number beyond 2^53 loses its last digits.">
          <Braces aria-hidden="true" />
          Format
        </Button>
        <Button type="button" variant="outline" size="sm" onClick={() => setChecked(true)}>
          <Check aria-hidden="true" />
          Validate
        </Button>
        <p id={status} role="status" className={cn("text-sm", problem ? "text-warning" : "text-muted-foreground")}>
          {problem ? `Not valid JSON: ${problem}` : checked ? (value.trim() === "" ? "Empty" : "Valid JSON") : ""}
        </p>
      </div>
    </div>
  );
}
