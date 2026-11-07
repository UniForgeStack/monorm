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
