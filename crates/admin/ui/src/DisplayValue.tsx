import { Check, Minus, X } from "lucide-react";
import { Badge } from "./components/ui/badge";
import type { Shown } from "./display";

/** A bool as an icon with a text label for screen readers, or a value as a badge. The colours come from the theme tokens. */
export function Display({ shown }: { shown: Shown }) {
  if (shown.kind === "badge") return <Badge variant="tone" className={`badge-${shown.colour}`}>{shown.text}</Badge>;
  const icon = { check: <Check className="size-4 text-success" />, x: <X className="size-4 text-muted-foreground" />, dash: <Minus className="size-4 text-muted-foreground" /> }[shown.icon];
  return <span role="img" aria-label={shown.label} className="inline-flex">{icon}</span>;
}
