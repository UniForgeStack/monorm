import * as TabsPrimitive from "@radix-ui/react-tabs";
import { createContext, useContext, type ComponentProps } from "react";
import { cn } from "../../lib/utils";

const Look = createContext<"line" | "pill">("line");

export const Tabs = TabsPrimitive.Root;
/** `variant="pill"`: the tabs sit in a rounded track and the active one is a raised pill. The default is the underline. */
export function TabsList({ className, variant = "line", ...props }: ComponentProps<typeof TabsPrimitive.List> & { variant?: "line" | "pill" }) {
  return (
    <Look.Provider value={variant}>
      <TabsPrimitive.List className={cn(variant === "pill" ? "inline-flex items-center gap-1 rounded-lg bg-muted p-1" : "inline-flex items-center gap-1 border-b border-border", className)} {...props} />
    </Look.Provider>
  );
}
export function TabsTrigger({ className, ...props }: ComponentProps<typeof TabsPrimitive.Trigger>) {
  const pill = useContext(Look) === "pill";
  return (
    <TabsPrimitive.Trigger
      className={cn(
        pill
          ? "rounded-md px-3 py-1.5 text-sm font-medium whitespace-nowrap text-muted-foreground hover:text-foreground focus-visible:outline-2 focus-visible:outline-ring data-[state=active]:bg-card data-[state=active]:text-foreground data-[state=active]:shadow-xs"
          : "-mb-px border-b-2 border-transparent px-3 py-2 text-sm font-medium whitespace-nowrap text-muted-foreground hover:text-foreground focus-visible:outline-2 focus-visible:outline-ring data-[state=active]:border-primary data-[state=active]:text-foreground",
        className,
      )}
      {...props}
    />
  );
}
export function TabsContent({ className, ...props }: ComponentProps<typeof TabsPrimitive.Content>) {
  return <TabsPrimitive.Content className={cn("pt-4 focus-visible:outline-2 focus-visible:outline-ring", className)} {...props} />;
}
