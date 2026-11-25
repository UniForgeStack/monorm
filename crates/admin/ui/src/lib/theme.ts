import { useSyncExternalStore } from "react";
import { faviconUrl, ramp } from "./ramp";

export type Mode = "light" | "dark" | "system";
const KEY = "monorm-admin-theme";
const modes: Mode[] = ["light", "dark", "system"];
const watchers = new Set<() => void>();
const media = window.matchMedia("(prefers-color-scheme: dark)");

const stored = (): Mode => {
  try {
    const v = window.localStorage.getItem(KEY);
    return modes.includes(v as Mode) ? (v as Mode) : "system";
  } catch {
    return "system";
  }
};
let mode = stored();

const paint = () => document.documentElement.classList.toggle("dark", mode === "dark" || (mode === "system" && media.matches));

/** Read the stored choice and set the `dark` class on <html>. Run it before the first render so the page does not flash. */
export function initTheme() {
  paint();
  media.addEventListener("change", paint);
}

export function setMode(next: Mode) {
  mode = next;
  try {
    window.localStorage.setItem(KEY, next);
  } catch {
    // A browser that refuses storage keeps the choice for this page only.
  }
  paint();
  watchers.forEach((fn) => fn());
}

export const useMode = () =>
  useSyncExternalStore(
    (fn) => (watchers.add(fn), () => void watchers.delete(fn)),
    () => mode,
  );

const rgb = (hex: string): [number, number, number] => {
  const h = hex.slice(1);
  const full = h.length === 3 ? [...h].map((c) => c + c).join("") : h;
  return [0, 2, 4].map((i) => Number.parseInt(full.slice(i, i + 2), 16)) as [number, number, number];
};
const channel = (v: number) => {
  const c = v / 255;
  return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
};
const luminance = ([r, g, b]: [number, number, number]) => 0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b);
/** The WCAG contrast ratio of two colours. */
export const contrast = (a: [number, number, number], b: [number, number, number]) => {
  const [x, y] = [luminance(a), luminance(b)];
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
};
const toHex = (c: number[]) => `#${c.map((v) => Math.round(v).toString(16).padStart(2, "0")).join("")}`;
const mix = (c: [number, number, number], to: number, t: number): [number, number, number] => [c[0] + (to - c[0]) * t, c[1] + (to - c[1]) * t, c[2] + (to - c[2]) * t];

/** The darkest and lightest surfaces that link text sits on, in the light and the dark theme. */
const LIGHT_SURFACE = rgb("#f6f7f9");
const DARK_SURFACE = rgb("#131a31");

/** Move `c` toward black (`to` 0) or white (`to` 255) until it reads on `surface` at 4.5:1. */
const readable = (c: [number, number, number], surface: [number, number, number], to: number) => {
  let out = c;
  for (let t = 0; contrast(out, surface) < 4.5 && t <= 1; t += 0.05) out = mix(c, to, t);
  return out;
};

/** The variables that a brand colour sets on <html>: the fill, the text on the fill and the colour of links in each theme. */
export function brandVars(color: string): Record<string, string> {
  const c = rgb(color);
  const white: [number, number, number] = [255, 255, 255];
  const ink: [number, number, number] = [11, 16, 32];
  return {
    "--brand": color,
    "--brand-foreground": contrast(c, white) >= contrast(c, ink) ? "#ffffff" : "#0b1020",
    "--brand-text": toHex(readable(c, LIGHT_SURFACE, 0)),
    "--brand-text-dark": toHex(readable(c, DARK_SURFACE, 255)),
  };
}

/** The `accent` surface of the site in each theme: the colour mixed into the surface, so the text that sits on it keeps its contrast. */
export function accentVars(color: string): Record<string, string> {
  const c = rgb(color);
  return {
    "--brand-accent": toHex(mix(c, 255, 0.82)),
    "--brand-accent-dark": toHex(mix(c, 0, 0.72)),
  };
}

/** Set the brand colour of the site on <html>. `setProperty` is a script call, so the CSP `style-src 'self'` allows it. */
export function applyBrand(colors: { primary?: string; accent?: string }, service = false) {
  const root = document.documentElement.style;
  // The colour of the current service sets the whole primary ramp; a site with only `colors.primary` keeps the one fill.
  if (colors.primary && service) {
    for (const [k, v] of Object.entries(ramp(colors.primary))) root.setProperty(k, v);
  }
  setFavicon(faviconUrl(colors.primary ?? "#1f4fd8"));
  if (colors.primary) for (const [k, v] of Object.entries(brandVars(colors.primary))) root.setProperty(k, v);
  if (colors.accent) for (const [k, v] of Object.entries(accentVars(colors.accent))) root.setProperty(k, v);
}

function setFavicon(href: string) {
  let link = document.querySelector<HTMLLinkElement>('link[rel="icon"]');
  if (!link) {
    link = document.createElement("link");
    link.rel = "icon";
    document.head.append(link);
  }
  link.type = "image/svg+xml";
  link.href = href;
}
