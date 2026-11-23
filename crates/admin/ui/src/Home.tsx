import { useQuery } from "@tanstack/react-query";
import { FolderOpen, History } from "lucide-react";
import { FrameView } from "./Frame";
import * as api from "./api";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "./components/ui/card";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "./components/ui/table";
import { cell, type Zone } from "./format";
import { Link, detailUrl, listUrl } from "./router";

const LOG = "AdminLogEntry";
const SHOWN = ["at", "user_key", "action", "resource", "record_label"];

/** The groups of the sidebar as cards, and the last audit rows that this user may read. */
export function Home({ site, zone }: { site: api.Site | undefined; zone: Zone }) {
  if (!site) return <p className="text-muted-foreground">Loading…</p>;
  const byName = new Map(site.resources.map((r) => [r.name, r]));
  return (
    <div className="grid grid-cols-1 gap-6">
      <h1 className="m-0">{site.brand.name}</h1>
      {site.nav.length === 0 && <p className="text-muted-foreground">No resource is available to this account.</p>}
      <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
        {site.nav.map((g) => (
          <Card key={g.group ?? ""}>
            <CardHeader>
              <CardTitle className="flex items-center gap-2">
                <FolderOpen className="size-4 text-muted-foreground" aria-hidden />
                {g.group ?? "Resources"}
              </CardTitle>
              <CardDescription>{g.resources.length === 1 ? "1 resource" : `${g.resources.length} resources`}</CardDescription>
            </CardHeader>
            <CardContent>
              <ul className="m-0 grid list-none gap-1 p-0">
                {g.resources.map((n) => (
                  <li key={n}>
                    <Link href={listUrl(n)} className="text-link">{byName.get(n)?.title ?? n}</Link>
                  </li>
                ))}
              </ul>
            </CardContent>
          </Card>
        ))}
      </div>
      {site.frame_cards?.map((frame) => (
        <Card key={frame.name}>
          <CardHeader><CardTitle>{frame.title}</CardTitle></CardHeader>
          <CardContent><FrameView frame={frame} /></CardContent>
        </Card>
      ))}
      {byName.has(LOG) && <Recent zone={zone} />}
    </div>
  );
}

function Recent({ zone }: { zone: Zone }) {
  const log = useQuery({ queryKey: ["recent-activity"], queryFn: () => api.page(LOG, "?limit=10&sort=-id"), staleTime: 15_000, retry: false });
  const page = log.data;
  const at = (label: string) => page?.columns.findIndex((c) => c.label === label) ?? -1;
  const shown = SHOWN.map((label) => ({ label, i: at(label) })).filter((c) => c.i >= 0);
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <History className="size-4 text-muted-foreground" aria-hidden />
          Recent activity
        </CardTitle>
        <CardDescription>The last 10 changes in the audit log.</CardDescription>
      </CardHeader>
      <CardContent>
        {log.isPending && <p className="text-sm text-muted-foreground">Loading…</p>}
        {log.isError && <p className="text-sm text-muted-foreground">The audit log is not available.</p>}
        {page && page.rows.length === 0 && <p className="text-sm text-muted-foreground">Nothing was changed yet.</p>}
        {page && page.rows.length > 0 && (
          <div className="overflow-x-auto">
            <Table>
              <TableHeader>
                <TableRow>
                  {shown.map((c) => (
                    <TableHead key={c.label}>{page.columns[c.i]?.title ?? c.label}</TableHead>
                  ))}
                </TableRow>
              </TableHeader>
              <TableBody>
                {page.rows.map((row, r) => (
                  <TableRow key={page.keys[r] ?? r}>
                    {shown.map((c, k) => (
                      <TableCell key={c.label}>
                        {k === 0 ? <Link href={detailUrl(LOG, page.keys[r] ?? "")} className="text-link">{cell(row[c.i], page.columns[c.i]!, zone)}</Link> : cell(row[c.i], page.columns[c.i]!, zone)}
                      </TableCell>
                    ))}
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        )}
      </CardContent>
    </Card>
  );
}
