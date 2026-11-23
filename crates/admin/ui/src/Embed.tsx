import type { MouseEvent } from "react";
import { useQuery } from "@tanstack/react-query";
import * as api from "./api";
import { DetailCards } from "./Detail";
import { getZone } from "./format";
import { PackageTab, widgetCall, widgetStream } from "./Packages";
import { BASE, LOGIN_URL, useSearch } from "./router";
import { Alert, message } from "./ui";

// The server marks the document. History changes inside a frame cannot mount the full admin.
export const embedSite = document.querySelector<HTMLMetaElement>('meta[name="monorm-admin-embed"]')?.content ?? null;
const name = window.location.pathname.slice(`${BASE}/-/embed/`.length);

function openLink(event: MouseEvent<HTMLElement>) {
  const link = event.target instanceof Element ? event.target.closest<HTMLAnchorElement>("a[href]") : null;
  if (!link || event.button > 1) return;
  event.preventDefault();
  event.stopPropagation();
  if (link.protocol === "https:" || link.protocol === "http:") {
    window.open(link.href, "_blank", "noopener,noreferrer");
  }
}

async function readOnly<T>(request: (method: string, path: string) => Promise<T>, method: string, path: string) {
  if (method.toUpperCase() !== "GET") throw new Error("Embedded tabs can only read.");
  return request("GET", path);
}

export function EmbedPage({ siteName }: { siteName: string }) {
  const search = useSearch();
  const q = useQuery({ queryKey: ["embed", name, search], queryFn: () => api.embed(name, search), retry: false });
  if (q.isPending) return <p className="muted p-4">Loading…</p>;
  if (q.error instanceof api.ApiError && q.error.status === 401) {
    return <p className="p-4"><a className="text-link" href={LOGIN_URL} target="_blank" rel="noopener noreferrer">Sign in to {siteName}</a></p>;
  }
  if (q.isError) return <main className="p-4"><Alert>{message(q.error)}</Alert></main>;
  const { title, key, tab } = q.data;
  const pkg = tab.package;
  return (
    <main className="p-4" onClickCapture={openLink} onAuxClickCapture={openLink} onSubmitCapture={(e) => e.preventDefault()}>
      {pkg && tab.name && tab.resource ? (
        <>
          <h1>{title}</h1>
          <PackageTab site={{ admin_major: pkg.admin_major, packages: [pkg] }} name={tab.name} resource={tab.resource} id={key} embedded
            record={Object.fromEntries(tab.fields.map((f, i) => [f.label, tab.values[i]]))}
            call={(method, path) => readOnly(widgetCall, method, path)} stream={(method, path) => readOnly(widgetStream, method, path)} />
        </>
      ) : tab.fields.length === 0 ? (
        <h1>{title}</h1>
      ) : (
        <DetailCards detail={{ ...tab, links: [] }} sections={undefined} zone={getZone()} title={title} />
      )}
    </main>
  );
}
