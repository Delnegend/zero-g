import { useCallback, useEffect, useMemo, useState } from "react";
import {
  Activity,
  ArrowDownUp,
  Clock3,
  KeyRound,
  RefreshCw,
  Search,
  Server,
  Settings,
} from "lucide-react";
import { Button } from "./components/ui/button";
import { Management } from "./Management";
import { Input } from "./components/ui/input";
import { Label } from "./components/ui/label";
import { Card } from "./components/ui/card";
import { Alert, AlertDescription } from "./components/ui/alert";
import {
  Table,
  TableHeader,
  TableBody,
  TableRow,
  TableHead,
  TableCell,
} from "./components/ui/table";
import {
  Sidebar,
  SidebarContent,
  SidebarGroup,
  SidebarHeader,
  SidebarInset,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarProvider,
  SidebarTrigger,
  useSidebar,
} from "./components/ui/sidebar";

type Log = {
  id: string | number;
  created_at: number;
  provider: string;
  model: string;
  path: string;
  status: number;
  latency_ms: number;
  input_tokens: number | null;
  output_tokens: number | null;
  request_body: unknown;
  response_body: unknown;
};

type Observability = {
  summary: {
    requests: number;
    successes: number;
    errors: number;
    input_tokens: number | null;
    output_tokens: number | null;
    avg_latency_ms: number;
  };
  logs: Log[];
};

const number = new Intl.NumberFormat();

function formatTokens(value: number | null): string {
  return value == null ? "Unknown" : number.format(value);
}

function formatBody(body: unknown): string {
  if (body == null) return "No body recorded";
  if (typeof body === "string") {
    try {
      return JSON.stringify(JSON.parse(body), null, 2);
    } catch {
      return body || "Empty body";
    }
  }
  return JSON.stringify(body, null, 2) ?? "No body recorded";
}

function formatDate(value: number): string {
  const date = new Date(value * 1000);
  return Number.isNaN(date.getTime()) ? String(value) : date.toLocaleString();
}

function ObservabilityPage() {
  const [data, setData] = useState<Observability | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [query, setQuery] = useState("");
  const [selectedId, setSelectedId] = useState<Log["id"] | null>(null);

  const load = useCallback(async (signal?: AbortSignal) => {
    setLoading(true);
    setError(null);
    try {
      const response = await fetch("/api/observability", { signal });
      if (!response.ok) throw new Error(`Request failed (${response.status})`);
      const result: Observability = await response.json();
      if (!signal?.aborted) setData(result);
    } catch (err) {
      if (!signal?.aborted)
        setError(
          err instanceof Error
            ? err.message
            : "Unable to load observability data.",
        );
    } finally {
      if (!signal?.aborted) setLoading(false);
    }
  }, []);

  useEffect(() => {
    const controller = new AbortController();
    void load(controller.signal);
    return () => controller.abort();
  }, [load]);

  const logs = useMemo(() => {
    const term = query.trim().toLowerCase();
    return (data?.logs ?? []).filter((log) =>
      [
        log.id,
        log.created_at,
        log.provider,
        log.model,
        log.path,
        log.status,
      ].some((value) =>
        String(value ?? "")
          .toLowerCase()
          .includes(term),
      ),
    );
  }, [data, query]);
  const selected = data?.logs.find((log) => log.id === selectedId);
  const summary = data?.summary;
  const cards = summary
    ? [
        {
          label: "Total requests",
          value: number.format(summary.requests),
          detail: "Recorded requests",
          icon: Activity,
        },
        {
          label: "Successful",
          value: number.format(summary.successes),
          detail: `${number.format(summary.errors)} errors`,
          icon: Activity,
        },
        {
          label: "Tokens",
          value:
            summary.input_tokens == null || summary.output_tokens == null
              ? "Unknown"
              : number.format(summary.input_tokens + summary.output_tokens),
          detail: `${formatTokens(summary.input_tokens)} in · ${formatTokens(summary.output_tokens)} out`,
          icon: ArrowDownUp,
        },
        {
          label: "Average latency",
          value: `${number.format(Math.round(summary.avg_latency_ms))} ms`,
          detail: "Across recorded requests",
          icon: Clock3,
        },
      ]
    : [];

  return (
    <main className="mx-auto max-w-7xl px-4 py-8 sm:px-6 lg:px-8 lg:py-12">
      <header className="mb-8 flex flex-wrap items-start justify-between gap-4">
        <div>
          <p className="mb-2 text-xs font-semibold uppercase tracking-widest text-muted-foreground">
            Zero-G
          </p>
          <h1 className="text-3xl font-semibold tracking-tight">
            Observability
          </h1>
          <p className="mt-2 text-sm text-muted-foreground">
            Monitor requests, usage, and responses across your gateway.
          </p>
        </div>
        <Button
          variant="outline"
          onClick={() => void load()}
          disabled={loading}
        >
          <RefreshCw aria-hidden="true" /> {loading ? "Loading…" : "Refresh"}
        </Button>
      </header>

      {error && (
        <Alert variant="destructive" className="mb-6">
          <AlertDescription>
            Unable to load observability data: {error}.{" "}
            <button
              type="button"
              className="font-medium underline underline-offset-4"
              onClick={() => void load()}
            >
              Try again
            </button>
          </AlertDescription>
        </Alert>
      )}
      {loading && !data && (
        <p
          role="status"
          className="py-12 text-center text-sm text-muted-foreground"
        >
          Loading observability data…
        </p>
      )}
      {summary && (
        <>
          <section
            aria-label="Request summary"
            className="mb-8 grid gap-4 sm:grid-cols-2 lg:grid-cols-4"
          >
            {cards.map(({ label, value, detail, icon: Icon }) => (
              <Card key={label} className="p-5">
                <div className="flex items-center justify-between gap-2 text-sm font-medium">
                  <h2>{label}</h2>
                  <Icon
                    className="size-4 text-muted-foreground"
                    aria-hidden="true"
                  />
                </div>
                <p className="mt-4 text-2xl font-semibold tracking-tight">
                  {value}
                </p>
                <p className="mt-1 text-xs text-muted-foreground">{detail}</p>
              </Card>
            ))}
          </section>
          <Card aria-labelledby="logs-heading" className="py-0">
            <div className="flex flex-wrap items-end justify-between gap-4 border-b p-5">
              <div>
                <h2 id="logs-heading" className="text-lg font-semibold">
                  Request logs
                </h2>
                <p className="mt-1 text-sm text-muted-foreground">
                  Select a request to inspect its details.
                </p>
              </div>
              <div className="relative w-full sm:w-72">
                <Search
                  aria-hidden="true"
                  className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground"
                />
                <Label className="sr-only" htmlFor="log-search">
                  Search request logs
                </Label>
                <Input
                  id="log-search"
                  type="search"
                  value={query}
                  onChange={(event) => setQuery(event.target.value)}
                  placeholder="Search logs…"
                  className="h-9 w-full rounded-lg border border-input bg-background pl-9 pr-3 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50"
                />
              </div>
            </div>
            <div className="overflow-x-auto">
              <Table className="w-full min-w-[720px] text-left text-sm">
                <TableHeader className="border-b text-xs text-muted-foreground">
                  <TableRow>
                    <TableHead className="px-5 py-3 font-medium" scope="col">
                      Time
                    </TableHead>
                    <TableHead className="px-5 py-3 font-medium" scope="col">
                      Provider / model
                    </TableHead>
                    <TableHead className="px-5 py-3 font-medium" scope="col">
                      Path
                    </TableHead>
                    <TableHead className="px-5 py-3 font-medium" scope="col">
                      Status
                    </TableHead>
                    <TableHead className="px-5 py-3 font-medium" scope="col">
                      Latency
                    </TableHead>
                    <TableHead className="px-5 py-3 font-medium" scope="col">
                      <span className="sr-only">Details</span>
                    </TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {logs.map((log) => (
                    <TableRow
                      key={log.id}
                      className="border-b last:border-0 hover:bg-muted/50"
                    >
                      <TableCell className="whitespace-nowrap px-5 py-4 text-muted-foreground">
                        {formatDate(log.created_at)}
                      </TableCell>
                      <TableCell className="px-5 py-4">
                        <div className="font-medium">{log.model}</div>
                        <div className="text-xs text-muted-foreground">
                          {log.provider}
                        </div>
                      </TableCell>
                      <TableCell className="px-5 py-4 font-mono text-xs">
                        {log.path}
                      </TableCell>
                      <TableCell className="px-5 py-4">
                        <span
                          className={`rounded-md px-2 py-1 text-xs font-medium ${log.status >= 400 ? "bg-destructive/10 text-destructive" : "bg-muted text-foreground"}`}
                        >
                          {log.status}
                        </span>
                      </TableCell>
                      <TableCell className="whitespace-nowrap px-5 py-4">
                        {number.format(log.latency_ms)} ms
                      </TableCell>
                      <TableCell className="px-5 py-4">
                        <Button
                          variant="outline"
                          size="sm"
                          aria-label={`View request ${log.id}`}
                          aria-pressed={selectedId === log.id}
                          onClick={() => setSelectedId(log.id)}
                        >
                          View
                        </Button>
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
              {logs.length === 0 && (
                <p className="px-5 py-10 text-center text-sm text-muted-foreground">
                  {query.trim()
                    ? "No requests match your search."
                    : "No requests recorded yet."}
                </p>
              )}
            </div>
          </Card>
          {selected && (
            <Card aria-labelledby="details-heading" className="mt-6 p-5">
              <div className="flex flex-wrap items-start justify-between gap-3">
                <div>
                  <h2 id="details-heading" className="text-lg font-semibold">
                    Request details
                  </h2>
                  <p className="mt-1 break-all text-sm text-muted-foreground">
                    {selected.provider} / {selected.model} ·{" "}
                    {formatDate(selected.created_at)} · ID {selected.id}
                  </p>
                </div>
                <Button variant="ghost" onClick={() => setSelectedId(null)}>
                  Close details
                </Button>
              </div>
              <dl className="mt-5 flex flex-wrap gap-x-8 gap-y-2 text-sm">
                <div>
                  <dt className="text-muted-foreground">Path</dt>
                  <dd className="font-mono">{selected.path}</dd>
                </div>
                <div>
                  <dt className="text-muted-foreground">Status</dt>
                  <dd>{selected.status}</dd>
                </div>
                <div>
                  <dt className="text-muted-foreground">Latency</dt>
                  <dd>{number.format(selected.latency_ms)} ms</dd>
                </div>
                <div>
                  <dt className="text-muted-foreground">Tokens</dt>
                  <dd>
                    {formatTokens(selected.input_tokens)} in ·{" "}
                    {formatTokens(selected.output_tokens)} out
                  </dd>
                </div>
              </dl>
              <div className="mt-5 grid min-w-0 gap-4 lg:grid-cols-2">
                {(
                  [
                    ["Request body", selected.request_body],
                    ["Response body", selected.response_body],
                  ] as const
                ).map(([title, body]) => (
                  <div key={title} className="min-w-0">
                    <h3 className="mb-2 text-sm font-medium">{title}</h3>
                    <pre className="max-h-96 overflow-auto rounded-lg border bg-muted/50 p-4 text-xs leading-relaxed">
                      <code>{formatBody(body)}</code>
                    </pre>
                  </div>
                ))}
              </div>
            </Card>
          )}
        </>
      )}
    </main>
  );
}

type Page = "observability" | "settings" | "keys" | "providers";

function DashboardSidebar({
  page,
  setPage,
}: {
  page: Page;
  setPage: (page: Page) => void;
}) {
  const { isMobile, setOpenMobile } = useSidebar();
  const items = [
    { id: "observability", label: "Observability", icon: Activity },
    { id: "settings", label: "Settings", icon: Settings },
    { id: "keys", label: "Virtual keys", icon: KeyRound },
    { id: "providers", label: "Providers", icon: Server },
  ] as const;
  return (
    <Sidebar collapsible="icon">
      <SidebarHeader className="px-4 py-5 font-semibold">Zero-G</SidebarHeader>
      <SidebarContent>
        <SidebarGroup>
          <nav aria-label="Dashboard">
            <SidebarMenu>
              {items.map(({ id, label, icon: Icon }) => (
                <SidebarMenuItem key={id}>
                  <SidebarMenuButton
                    isActive={page === id}
                    aria-current={page === id ? "page" : undefined}
                    tooltip={label}
                    onClick={() => {
                      setPage(id);
                      if (isMobile) setOpenMobile(false);
                    }}
                  >
                    <Icon aria-hidden="true" />
                    <span>{label}</span>
                  </SidebarMenuButton>
                </SidebarMenuItem>
              ))}
            </SidebarMenu>
          </nav>
        </SidebarGroup>
      </SidebarContent>
    </Sidebar>
  );
}

function App() {
  const [page, setPage] = useState<Page>("observability");
  return (
    <SidebarProvider>
      <DashboardSidebar page={page} setPage={setPage} />
      <SidebarInset>
        <header className="flex h-12 items-center border-b px-4">
          <SidebarTrigger />
        </header>
        {page === "observability" ? (
          <ObservabilityPage />
        ) : (
          <Management page={page} />
        )}
      </SidebarInset>
    </SidebarProvider>
  );
}

export default App;
