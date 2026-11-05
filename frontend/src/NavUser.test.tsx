import { afterEach, beforeEach, test } from "node:test";
import assert from "node:assert/strict";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, cleanup } from "@testing-library/react/pure";
import userEvent from "@testing-library/user-event";
import { NavUser } from "./components/NavUser";
import { SidebarProvider } from "./components/ui/sidebar";

let queryClient: QueryClient;

beforeEach(() => {
  document.body.innerHTML = "";
  Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", {
    value: true,
    configurable: true,
    writable: true,
  });
  queryClient = new QueryClient({
    defaultOptions: {
      queries: { retry: false, gcTime: 0 },
    },
  });
});

afterEach(() => {
  cleanup();
  queryClient.clear();
  document.body.innerHTML = "";
});

test("renders user info, profile picture, and only the logout button in menu", async () => {
  const user = userEvent.setup();
  let loggedOut = false;
  const customUser = {
    name: "Alex Doe",
    email: "alex@example.com",
    avatar: "https://example.com/photo.jpg",
  };

  const { getByRole, findByText, queryByText } = render(
    <QueryClientProvider client={queryClient}>
      <SidebarProvider>
        <NavUser
          user={customUser}
          onLogout={() => {
            loggedOut = true;
          }}
        />
      </SidebarProvider>
    </QueryClientProvider>,
  );

  const button = getByRole("button");
  assert.ok(button.textContent?.includes("Alex Doe"));
  assert.ok(button.textContent?.includes("alex@example.com"));

  await user.click(button);

  // Only the logout button should remain in the dropdown menu
  assert.equal(queryByText("Account"), null);
  assert.equal(queryByText("Billing"), null);
  assert.equal(queryByText("Notifications"), null);

  const logoutButton = await findByText("Log out");
  assert.ok(logoutButton);

  await user.click(logoutButton);
  assert.equal(loggedOut, true);
});

test("wires display name, email and avatar fallback initials when picture is absent", async () => {
  const customUser = {
    displayName: "Taylor Swift",
    email: "taylor@example.com",
    avatar: "",
  };

  const { getByRole } = render(
    <QueryClientProvider client={queryClient}>
      <SidebarProvider>
        <NavUser user={customUser} />
      </SidebarProvider>
    </QueryClientProvider>,
  );

  const button = getByRole("button");
  assert.ok(button.textContent?.includes("Taylor Swift"));
  assert.ok(button.textContent?.includes("taylor@example.com"));
  assert.ok(button.textContent?.includes("TS"));
});
