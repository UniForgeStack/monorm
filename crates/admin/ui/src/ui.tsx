// The page's small set of helpers. They sit on the kit (components/ui) so a screen keeps one look.
import type { InputHTMLAttributes, ReactNode } from "react";
import { cn } from "./lib/utils";
import { Button as KitButton, type ButtonProps } from "./components/ui/button";
import { Card as KitCard, CardContent, CardHeader, CardTitle } from "./components/ui/card";
import { Input } from "./components/ui/input";
import { Label } from "./components/ui/label";

/** `primary` is the filled button, `plain` the outlined one. */
export function Button({ variant = "primary", ...rest }: Omit<ButtonProps, "variant"> & { variant?: "primary" | "plain" | "destructive" | "ghost" }) {
  const map = { primary: "default", plain: "outline", destructive: "destructive", ghost: "ghost" } as const;
  return <KitButton variant={map[variant]} {...rest} />;
}

export function Field({ label, className, ...rest }: InputHTMLAttributes<HTMLInputElement> & { label: string }) {
  const id = `f-${rest.name ?? label}`;
  return (
    <div className="mb-3 grid gap-1.5">
      <Label htmlFor={id}>{label}</Label>
      <Input id={id} className={className} {...rest} />
    </div>
  );
}

export function Alert({ children, kind = "error" }: { children: ReactNode; kind?: "error" | "ok" }) {
  return (
    <p
      className={cn("my-2 rounded-md border px-3 py-2 text-sm", kind === "error" ? "border-destructive/50 bg-destructive/10 text-destructive" : "border-success/50 bg-success/10 text-success")}
      role={kind === "error" ? "alert" : "status"}
    >
      {children}
    </p>
  );
}

export function Card({ title, children }: { title: string; children: ReactNode }) {
  return (
    <KitCard className="w-full max-w-sm">
      <CardHeader>
        <CardTitle className="text-lg">{title}</CardTitle>
      </CardHeader>
      <CardContent>{children}</CardContent>
    </KitCard>
  );
}

/** One checkbox for each read default that this user may switch off: `?without=a,b` in the URL. */
export function Bypass({ names, params, change }: { names: string[]; params: URLSearchParams; change: (edit: (p: URLSearchParams) => void, keepPosition?: boolean) => void }) {
  const off = (params.get("without") ?? "").split(",").filter((n) => n !== "");
  const toggle = (name: string, on: boolean) =>
    change((p) => {
      const next = names.filter((n) => (n === name ? on : off.includes(n)));
      if (next.length > 0) p.set("without", next.join(","));
      else p.delete("without");
    });
  return (
    <>
      {names.map((n) => (
        <label key={n} className="flex items-center gap-2 text-sm text-muted-foreground">
          <input type="checkbox" className="size-4 accent-primary" checked={off.includes(n)} onChange={(e) => toggle(n, e.target.checked)} />
          Show {n}
        </label>
      ))}
    </>
  );
}

export const message = (e: unknown) => (e instanceof Error ? e.message : String(e));
