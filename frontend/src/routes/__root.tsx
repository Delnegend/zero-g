import {
  createRootRoute,
  HeadContent,
  Link,
  Outlet,
  useRouterState,
} from "@tanstack/react-router";
import { Activity, Boxes, KeyRound, Moon, Settings, Sun } from "lucide-react";
import { useEffect, useState } from "react";
import { Button } from "@/components/ui/button";
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
} from "@/components/ui/sidebar";

export const Route = createRootRoute({ component: App });

function DashboardSidebar() {
  const path = useRouterState({ select: (state) => state.location.pathname });
  const { isMobile, setOpenMobile, state, setOpen } = useSidebar();
  const [observabilityOpen, setObservabilityOpen] = useState(true);
  const [modelsOpen, setModelsOpen] = useState(true);
  const items = [
    { path: "/keys", label: "Virtual keys", icon: KeyRound },
    { path: "/settings", label: "Settings", icon: Settings },
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
              <SidebarMenuItem>
                <SidebarMenuButton
                  isActive={path.startsWith("/models/")}
                  aria-expanded={modelsOpen}
                  tooltip="Models"
                  onClick={() => {
                    if (state === "collapsed" && !isMobile) {
                      setOpen(true);
                      setModelsOpen(true);
                    } else {
                      setModelsOpen(!modelsOpen);
                    }
                  }}
                >
                  <Boxes aria-hidden="true" />
                  <span>Models</span>
                </SidebarMenuButton>
                {modelsOpen && (
                  <SidebarMenuSub>
                    {(
                      [
                        ["/models/catalog", "Catalog"],
                        ["/models/providers", "Providers"],
                        ["/models/settings", "Settings"],
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
      <HeadContent />
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
