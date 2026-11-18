import type { HTMLAttributes, TdHTMLAttributes, ThHTMLAttributes } from "react";
import { cn } from "../../lib/utils";

export function Table({ className, ...props }: HTMLAttributes<HTMLTableElement>) {
  return (
    <div className="w-full overflow-x-auto rounded-lg border border-border bg-card">
      <table className={cn("w-full border-collapse text-sm", className)} {...props} />
    </div>
  );
}
export const TableHeader = ({ className, ...props }: HTMLAttributes<HTMLTableSectionElement>) => <thead className={cn("bg-muted/50", className)} {...props} />;
export const TableBody = ({ className, ...props }: HTMLAttributes<HTMLTableSectionElement>) => <tbody className={cn(className)} {...props} />;
export const TableRow = ({ className, ...props }: HTMLAttributes<HTMLTableRowElement>) => (
  <tr className={cn("border-b border-border last:border-b-0 hover:bg-accent/50 aria-selected:bg-accent", className)} {...props} />
);
export const TableHead = ({ className, ...props }: ThHTMLAttributes<HTMLTableCellElement>) => (
  <th className={cn("h-10 px-3 text-left align-middle text-xs font-medium whitespace-nowrap text-muted-foreground", className)} {...props} />
);
export const TableCell = ({ className, ...props }: TdHTMLAttributes<HTMLTableCellElement>) => (
  <td className={cn("px-3 py-2 align-middle whitespace-nowrap", className)} {...props} />
);
