import * as CollapsiblePrimitive from "@radix-ui/react-collapsible";
import { ChevronRight } from "lucide-react";
import type { ReactNode } from "react";
import { cn } from "../../lib/utils";

/** A section that opens and closes under its title. The trigger is a button with `aria-expanded`; the closed body stays mounted but hidden, so a field keeps its state. */
export function Collapsible({ title, open, onOpenChange, className, titleClassName = "text-base font-semibold", children }: { title: ReactNode; open: boolean; onOpenChange: (open: boolean) => void; className?: string; titleClassName?: string; children: ReactNode }) {
  return (
    <CollapsiblePrimitive.Root open={open} onOpenChange={onOpenChange} className={className}>
      <CollapsiblePrimitive.Trigger className={cn("group flex w-full cursor-pointer items-center gap-2 rounded-sm bg-transparent p-0 text-left text-foreground focus-visible:outline-2 focus-visible:outline-ring", titleClassName)}>
        <ChevronRight aria-hidden="true" className="size-4 shrink-0 transition-transform group-data-[state=open]:rotate-90" />
        {title}
      </CollapsiblePrimitive.Trigger>
      <CollapsiblePrimitive.Content forceMount className="mt-4 data-[state=closed]:hidden">{children}</CollapsiblePrimitive.Content>
    </CollapsiblePrimitive.Root>
  );
}
