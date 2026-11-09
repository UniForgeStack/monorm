// The own small component set. Radix gives the behavior of the menu and the label; the rest is plain markup.
import * as Label from "@radix-ui/react-label";
import type { ButtonHTMLAttributes, InputHTMLAttributes, ReactNode } from "react";

export function Button({ variant = "primary", ...rest }: ButtonHTMLAttributes<HTMLButtonElement> & { variant?: "primary" | "plain" }) {
  return <button className={`btn btn-${variant}`} {...rest} />;
}

export function Field({ label, ...rest }: InputHTMLAttributes<HTMLInputElement> & { label: string }) {
  const id = `f-${rest.name ?? label}`;
  return (
    <div className="field">
      <Label.Root htmlFor={id}>{label}</Label.Root>
      <input id={id} {...rest} />
    </div>
  );
}

export function Alert({ children, kind = "error" }: { children: ReactNode; kind?: "error" | "ok" }) {
  return (
    <p className={`alert alert-${kind}`} role={kind === "error" ? "alert" : "status"}>
      {children}
    </p>
  );
}

export function Card({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="card">
      <h1>{title}</h1>
      {children}
    </section>
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
        <label key={n} className="bypass">
          <input type="checkbox" checked={off.includes(n)} onChange={(e) => toggle(n, e.target.checked)} />
          Show {n}
        </label>
      ))}
    </>
  );
}

export const message = (e: unknown) => (e instanceof Error ? e.message : String(e));
