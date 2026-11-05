import { Avatar, AvatarFallback, AvatarImage } from "@/components/ui/avatar";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  useSidebar,
} from "@/components/ui/sidebar";
import { useQuery } from "@tanstack/react-query";
import { LogOut, MoreVertical } from "lucide-react";
import {
  currentUser as fetchCurrentUser,
  getLogoutUrl,
} from "@/server/api/generated/client";
import type { DashboardUser } from "@/server/api/generated/schemas";

export type OidcUser = Partial<DashboardUser> & {
  displayName?: string | null;
  picture?: string | null;
};

export type NavUserProps = {
  user?: OidcUser;
  onLogout?: () => void | Promise<void>;
};

export function NavUser({
  user: userProp,
  onLogout: onLogoutProp,
}: NavUserProps = {}) {
  const { isMobile } = useSidebar();

  const { data: fetchedUser } = useQuery({
    queryKey: ["auth-user"],
    queryFn: async ({ signal }) => {
      try {
        return await fetchCurrentUser({ signal });
      } catch {
        // Fall back gracefully when unauthenticated or endpoint is unavailable.
        return null;
      }
    },
    enabled: userProp === undefined,
    staleTime: 5 * 60 * 1000,
    retry: false,
  });

  const currentUser: OidcUser | null | undefined = userProp ?? fetchedUser;

  const displayName =
    currentUser?.name ||
    currentUser?.displayName ||
    (currentUser?.email ? currentUser.email.split("@")[0] : "") ||
    "User";

  const email = currentUser?.email || "";
  const avatarUrl = currentUser?.avatar || currentUser?.picture || "";

  const initials =
    displayName
      .split(" ")
      .map((part: string) => part[0])
      .filter(Boolean)
      .join("")
      .slice(0, 2)
      .toUpperCase() || (email ? email.slice(0, 2).toUpperCase() : "U");

  const handleLogout = () => {
    if (onLogoutProp) {
      void onLogoutProp();
      return;
    }
    window.location.assign(getLogoutUrl());
  };

  return (
    <SidebarMenu>
      <SidebarMenuItem>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <SidebarMenuButton
              size="lg"
              className="data-[state=open]:bg-sidebar-accent data-[state=open]:text-sidebar-accent-foreground"
            >
              <Avatar className="h-8 w-8 rounded-lg grayscale">
                {avatarUrl ? (
                  <AvatarImage src={avatarUrl} alt={displayName} />
                ) : null}
                <AvatarFallback className="rounded-lg">
                  {initials}
                </AvatarFallback>
              </Avatar>
              <div className="grid flex-1 text-left text-sm leading-tight">
                <span className="truncate font-medium">{displayName}</span>
                {email ? (
                  <span className="truncate text-xs text-muted-foreground">
                    {email}
                  </span>
                ) : null}
              </div>
              <MoreVertical className="ml-auto size-4" />
            </SidebarMenuButton>
          </DropdownMenuTrigger>
          <DropdownMenuContent
            className="w-(--anchor-width) min-w-56 rounded-lg"
            side={isMobile ? "bottom" : "right"}
            align="end"
            sideOffset={4}
          >
            <DropdownMenuLabel className="p-0 font-normal">
              <div className="flex items-center gap-2 px-1 py-1.5 text-left text-sm">
                <Avatar className="h-8 w-8 rounded-lg">
                  {avatarUrl ? (
                    <AvatarImage src={avatarUrl} alt={displayName} />
                  ) : null}
                  <AvatarFallback className="rounded-lg">
                    {initials}
                  </AvatarFallback>
                </Avatar>
                <div className="grid flex-1 text-left text-sm leading-tight">
                  <span className="truncate font-medium">{displayName}</span>
                  {email ? (
                    <span className="truncate text-xs text-muted-foreground">
                      {email}
                    </span>
                  ) : null}
                </div>
              </div>
            </DropdownMenuLabel>
            <DropdownMenuSeparator />
            <DropdownMenuItem onClick={handleLogout} className="cursor-pointer">
              <LogOut />
              <span>Log out</span>
            </DropdownMenuItem>
          </DropdownMenuContent>
        </DropdownMenu>
      </SidebarMenuItem>
    </SidebarMenu>
  );
}
