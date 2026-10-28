import { useCallback, useEffect, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  createRootRoute,
  createRoute,
  createRouter,
  Link,
  Outlet,
  RouterProvider,
  useRouterState,
} from "@tanstack/react-router";
import {
  Activity,
  ArrowDownUp,
  Clock3,
  KeyRound,
  Moon,
  RefreshCw,
  Server,
  Settings,
  Sun,
} from "lucide-react";
import { Button } from "./components/ui/button";
import { Management } from "./Management";
import { Card } from "./components/ui/card";
import { LogsTable } from "./LogsTable";
import { ObservabilityCharts } from "./ObservabilityCharts";
import { RequestBody, requestBodyTheme } from "./RequestBody";
import { Alert, AlertDescription } from "./components/ui/alert";
import {
  Drawer,
  DrawerClose,
  DrawerContent,
  DrawerDescription,
  DrawerFooter,
  DrawerHeader,
  DrawerTitle,
} from "./components/ui/drawer";
import {
  Sidebar,
  SidebarContent,
  SidebarGroup,
  SidebarHeader,
  SidebarInset,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarMenuSub,
  SidebarMenuSubButton,
  SidebarMenuSubItem,
  SidebarProvider,
  SidebarTrigger,
  useSidebar,
} from "./components/ui/sidebar";

export type Log = {
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

function formatDate(value: number): string {
  const date = new Date(value * 1000);
  return Number.isNaN(date.getTime()) ? String(value) : date.toLocaleString();
}

function ObservabilityPage({ logsOnly = false }: { logsOnly?: boolean }) {
  const {
    data,
    error: queryError,
    isFetching: loading,
    refetch,
  } = useQuery({
    queryKey: ["observability"],
    queryFn: async ({ signal }): Promise<Observability> => {
      const response = await fetch("/api/observability", { signal });
      if (!response.ok) throw new Error(`Request failed (${response.status})`);
      return response.json();
    },
    retry: false,
    gcTime: 0,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  });
  const error =
    !loading && queryError
      ? queryError instanceof Error
        ? queryError.message
        : "Unable to load observability data."
      : null;
  const [selectedId, setSelectedId] = useState<Log["id"] | null>(null);
  const detailsTrigger = useRef<HTMLButtonElement>(null);
  const selectLog = useCallback((id: Log["id"], trigger: HTMLButtonElement) => {
    detailsTrigger.current = trigger;
    setSelectedId(id);
  }, []);

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
    <main className="mx-auto w-full min-w-0 max-w-7xl px-4 py-8 sm:px-6 lg:px-8 lg:py-12">
      <header className="mb-8 flex flex-wrap items-start justify-between gap-4">
        <div>
          <p className="mb-2 text-xs font-semibold uppercase tracking-widest text-muted-foreground">
            Zero-G
          </p>
          <h1 className="text-3xl font-semibold tracking-tight">
            {logsOnly ? "LLM Logs" : "Dashboard"}
          </h1>
          <p className="mt-2 text-sm text-muted-foreground">
            {logsOnly
              ? "Search and inspect recorded LLM requests."
              : "Monitor requests, usage, and responses across your gateway."}
          </p>
        </div>
        <Button
          variant="outline"
          onClick={() => void refetch()}
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
              onClick={() => void refetch()}
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
          {!logsOnly && (
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
          )}
          {!logsOnly && <ObservabilityCharts logs={data?.logs ?? []} />}
          <LogsTable logs={data?.logs ?? []} onSelect={selectLog} />
          <Drawer
            open={selected != null}
            onOpenChange={(open) => {
              if (!open) setSelectedId(null);
            }}
            swipeDirection="right"
            showSwipeHandle
          >
            <DrawerContent
              finalFocus={detailsTrigger}
              className="data-[swipe-axis=x]:[--drawer-content-width:min(100vw,48rem)] data-[swipe-axis=x]:sm:[--drawer-content-width:min(100vw,48rem)]"
            >
              {selected && (
                <>
                  <DrawerHeader>
                    <DrawerTitle>Request details</DrawerTitle>
                    <DrawerDescription className="break-all">
                      {selected.provider} / {selected.model} ·{" "}
                      {formatDate(selected.created_at)} · ID {selected.id}
                    </DrawerDescription>
                  </DrawerHeader>
                  <div className="min-h-0 overflow-y-auto overscroll-contain p-4">
                    <dl className="flex min-w-0 flex-wrap gap-x-8 gap-y-2 text-sm">
                      <div className="min-w-0 max-w-full">
                        <dt className="text-muted-foreground">Path</dt>
                        <dd className="break-all font-mono">{selected.path}</dd>
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
                    <style>{requestBodyTheme}</style>
                    <div className="mt-5 grid min-w-0 grid-cols-1 gap-4 lg:grid-cols-2">
                      {(
                        [
                          ["Request body", selected.request_body],
                          ["Response body", selected.response_body],
                        ] as const
                      ).map(([title, body]) => (
                        <div key={title} className="min-w-0">
                          <h3 className="mb-2 text-sm font-medium">{title}</h3>
                          <RequestBody body={body} />
                        </div>
                      ))}
                    </div>
                  </div>
                  <DrawerFooter>
                    <DrawerClose render={<Button variant="outline" />}>
                      Close
                    </DrawerClose>
                  </DrawerFooter>
                </>
              )}
            </DrawerContent>
          </Drawer>
        </>
      )}
    </main>
  );
}

function DashboardSidebar() {
  const path = useRouterState({ select: (state) => state.location.pathname });
  const { isMobile, setOpenMobile, state, setOpen } = useSidebar();
  const [observabilityOpen, setObservabilityOpen] = useState(true);
  const items = [
    { path: "/settings", label: "Settings", icon: Settings },
    { path: "/keys", label: "Virtual keys", icon: KeyRound },
    { path: "/providers", label: "Providers", icon: Server },
  ] as const;
  return (
    <Sidebar collapsible="icon">
      <SidebarHeader
        aria-label="Zero-G"
        className="overflow-hidden px-4 py-5 font-semibold whitespace-nowrap"
      >
        <span className="grid" aria-hidden="true">
          <span
            className={`col-start-1 row-start-1 transition-opacity duration-150 motion-reduce:transition-none ${state === "collapsed" && !isMobile ? "opacity-100" : "opacity-0"}`}
          >
            0G
          </span>
          <span
            className={`col-start-1 row-start-1 transition-opacity duration-150 motion-reduce:transition-none ${state === "collapsed" && !isMobile ? "opacity-0" : "opacity-100 delay-150 motion-reduce:delay-0"}`}
          >
            Zero-G
          </span>
        </span>
      </SidebarHeader>
      <SidebarContent>
        <SidebarGroup>
          <nav aria-label="Dashboard">
            <SidebarMenu>
              <SidebarMenuItem>
                <SidebarMenuButton
                  isActive={path === "/" || path === "/llm-logs"}
                  aria-expanded={observabilityOpen}
                  tooltip="Observability"
                  onClick={() => {
                    if (state === "collapsed" && !isMobile) {
                      setOpen(true);
                      setObservabilityOpen(true);
                    } else {
                      setObservabilityOpen(!observabilityOpen);
                    }
                  }}
                >
                  <Activity aria-hidden="true" />
                  <span>Observability</span>
                </SidebarMenuButton>
                {observabilityOpen && (
                  <SidebarMenuSub>
                    {(
                      [
                        ["/", "Dashboard"],
                        ["/llm-logs", "LLM Logs"],
                      ] as const
                    ).map(([to, label]) => (
                      <SidebarMenuSubItem key={to}>
                        <SidebarMenuSubButton
                          render={<Link to={to} />}
                          isActive={path === to}
                          aria-current={path === to ? "page" : undefined}
                          onClick={() => {
                            if (isMobile) setOpenMobile(false);
                          }}
                        >
                          <span>{label}</span>
                        </SidebarMenuSubButton>
                      </SidebarMenuSubItem>
                    ))}
                  </SidebarMenuSub>
                )}
              </SidebarMenuItem>
              {items.map(({ path: to, label, icon: Icon }) => (
                <SidebarMenuItem key={to}>
                  <SidebarMenuButton
                    render={<Link to={to} />}
                    isActive={path === to}
                    aria-current={path === to ? "page" : undefined}
                    tooltip={label}
                    onClick={() => {
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
  const [dark, setDark] = useState(() => {
    const saved = localStorage.getItem("zero-g-theme");
    return (
      saved === "dark" ||
      (saved !== "light" &&
        window.matchMedia("(prefers-color-scheme: dark)").matches)
    );
  });
  useEffect(() => {
    document.documentElement.classList.toggle("dark", dark);
    localStorage.setItem("zero-g-theme", dark ? "dark" : "light");
  }, [dark]);
  return (
    <SidebarProvider>
      <DashboardSidebar />
      <SidebarInset className="min-w-0">
        <header className="flex h-12 items-center justify-between border-b px-4">
          <SidebarTrigger />
          <Button
            variant="ghost"
            size="icon"
            aria-label={dark ? "Switch to light mode" : "Switch to dark mode"}
            onClick={() => setDark(!dark)}
          >
            {dark ? <Sun aria-hidden="true" /> : <Moon aria-hidden="true" />}
          </Button>
        </header>
        <Outlet />
      </SidebarInset>
    </SidebarProvider>
  );
}

const rootRoute = createRootRoute({ component: App });
const routeTree = rootRoute.addChildren([
  createRoute({
    getParentRoute: () => rootRoute,
    path: "/",
    component: () => <ObservabilityPage />,
  }),
  createRoute({
    getParentRoute: () => rootRoute,
    path: "/llm-logs",
    component: () => <ObservabilityPage logsOnly />,
  }),
  createRoute({
    getParentRoute: () => rootRoute,
    path: "/settings",
    component: () => <Management page="settings" />,
  }),
  createRoute({
    getParentRoute: () => rootRoute,
    path: "/keys",
    component: () => <Management page="keys" />,
  }),
  createRoute({
    getParentRoute: () => rootRoute,
    path: "/providers",
    component: () => <Management page="providers" />,
  }),
]);
const router = createRouter({ routeTree });

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}

export default function AppRouter() {
  return <RouterProvider router={router} />;
}
