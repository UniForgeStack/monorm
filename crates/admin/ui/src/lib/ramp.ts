// Pure colour and chip rules of the shell: no DOM, so `npm test` runs them on Node.

/** Lightness (%) and chroma of each ramp step. Only the hue changes with the service colour, so the contrast of every step stays fixed. */
export const STEPS: Record<string, [number, number]> = {
  "50": [97.7, 0.014],
  "100": [94.6, 0.033],
  "200": [90.2, 0.063],
  "300": [82.7, 0.119],
  "400": [71.4, 0.203],
  "500": [62.7, 0.265],
  "600": [55.8, 0.288],
  "700": [49.6, 0.265],
  "800": [43.8, 0.218],
  "900": [38.1, 0.176],
  "950": [29.1, 0.149],
};

const linear = (v: number) => {
  const c = v / 255;
  return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
};

/** The OKLCH hue (degrees, one decimal) of `#rgb` or `#rrggbb`. A grey has hue 0. */
export function hueOf(hex: string): number {
  const h = hex.replace(/^#/, "");
  const full = h.length === 3 ? [...h].map((c) => c + c).join("") : h;
  const [r = 0, g = 0, b = 0] = [0, 2, 4].map((i) => linear(Number.parseInt(full.slice(i, i + 2), 16)));
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
  const a = 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s;
  const c = 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s;
  if (Math.hypot(a, c) < 1e-4) return 0;
  return Math.round(((Math.atan2(c, a) * 180) / Math.PI + 360) * 10) / 10 % 360;
}

/** The ramp of a colour: `--primary-50` to `--primary-950` as `oklch(L% C hue)`. */
export function ramp(hex: string): Record<string, string> {
  const hue = hueOf(hex);
  return Object.fromEntries(Object.entries(STEPS).map(([step, [l, c]]) => [`--primary-${step}`, `oklch(${l}% ${c} ${hue})`]));
}

export type Level = "info" | "warning" | "danger";

/** The level of the environment chip: the level the site sets, else the one the label implies (case ignored). */
export function envLevel(label: string, level?: string | null): Level {
  if (level === "info" || level === "warning" || level === "danger") return level;
  const l = label.trim().toLowerCase();
  if (l === "production" || l === "prod") return "danger";
  if (l === "staging") return "warning";
  return "info";
}

/** The favicon: three stacked squares in steps 400, 500 and 600 of the ramp, as an SVG data URL. */
export function faviconUrl(hex: string): string {
  const r = ramp(hex);
  const sq = (y: number, step: string) => `<rect x="4" y="${y}" width="24" height="9" rx="2" fill="${r[`--primary-${step}`]}"/>`;
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">${sq(3, "400")}${sq(11.5, "500")}${sq(20, "600")}</svg>`;
  return `data:image/svg+xml,${encodeURIComponent(svg)}`;
}

/** Initials of a name or an address: up to two letters. */
export function initials(text: string): string {
  const words = text.split(/[\s@._-]+/).filter(Boolean);
  const [a = "?", b] = words;
  return (b ? a.charAt(0) + b.charAt(0) : a.slice(0, 2)).toUpperCase();
}
