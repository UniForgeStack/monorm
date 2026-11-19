import type { InputHTMLAttributes } from "react";
import { cn } from "../../lib/utils";

/** A boolean as a switch. It is a native checkbox (role `switch`), so labels, forms and the keyboard work with no script. */
export function Switch({ className, ...props }: Omit<InputHTMLAttributes<HTMLInputElement>, "type">) {
  return (
    <input
      type="checkbox"
      role="switch"
      className={cn(
        "relative h-6 w-11 shrink-0 appearance-none rounded-full border border-input bg-muted transition-colors checked:border-primary checked:bg-primary disabled:cursor-not-allowed disabled:opacity-50 aria-[invalid=true]:border-destructive focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-ring forced-colors:checked:bg-[Highlight] forced-colors:before:bg-[ButtonText] forced-colors:checked:before:bg-[HighlightText]",
        "before:absolute before:left-0.5 before:top-0.5 before:size-4.5 before:rounded-full before:bg-foreground/70 before:transition-transform before:content-[''] checked:before:translate-x-5 checked:before:bg-primary-foreground",
        className,
      )}
      {...props}
    />
  );
}
