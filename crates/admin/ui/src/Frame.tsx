import type { Frame } from "./api";

export function FrameView({ frame }: { frame: Frame }) {
  if (!frame.url) return <p className="muted">Not available for this record</p>;
  return (
    <div className="grid gap-2">
      <a className="text-link justify-self-start" href={frame.url} target="_blank" rel="noopener noreferrer">Open in new tab</a>
      <iframe className="w-full rounded-md border border-border" src={frame.url} sandbox={frame.sandbox}
        referrerPolicy={frame.referrer_policy} loading="lazy" title={frame.title} style={{ height: frame.height }} />
    </div>
  );
}
