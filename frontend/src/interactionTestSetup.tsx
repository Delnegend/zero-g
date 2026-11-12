import { afterEach } from "node:test";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

Object.defineProperty(globalThis, "scrollTo", {
  value() {},
  configurable: true,
});
Object.defineProperty(globalThis, "IS_REACT_ACT_ENVIRONMENT", {
  value: true,
  configurable: true,
  writable: true,
});
Object.defineProperty(window, "matchMedia", {
  value: () => ({
    matches: false,
    addEventListener() {},
    removeEventListener() {},
    addListener() {},
    removeListener() {},
  }),
});
class ObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}
Object.defineProperty(globalThis, "ResizeObserver", {
  value: ObserverStub,
  configurable: true,
});
Object.defineProperty(window, "ResizeObserver", {
  value: ObserverStub,
  configurable: true,
});
Object.defineProperty(globalThis, "IntersectionObserver", {
  value: ObserverStub,
  configurable: true,
});
Object.defineProperty(window, "IntersectionObserver", {
  value: ObserverStub,
  configurable: true,
});
Object.defineProperty(HTMLElement.prototype, "scrollIntoView", {
  value() {},
  configurable: true,
});
Object.defineProperty(globalThis, "requestAnimationFrame", {
  value: (callback: FrameRequestCallback) =>
    setTimeout(() => callback(Date.now()), 0),
  configurable: true,
});
Object.defineProperty(globalThis, "cancelAnimationFrame", {
  value: clearTimeout,
  configurable: true,
});
Object.defineProperty(window, "requestAnimationFrame", {
  value: globalThis.requestAnimationFrame,
  configurable: true,
});
Object.defineProperty(window, "cancelAnimationFrame", {
  value: globalThis.cancelAnimationFrame,
  configurable: true,
});

// These modules capture the browser document at import time, so load them after JSDOM setup.
export const {
  act,
  fireEvent,
  render,
  within,
  cleanup,
  waitFor,
  getQueriesForElement,
} = await import("@testing-library/react");
export const screen = getQueriesForElement(document.body);
export const userEvent = (await import("@testing-library/user-event")).default;
export const { SettingsPage } = await import("./routes/settings");
export const { KeysPage } = await import("./routes/keys");
export const { routeTree } = await import("./routeTree.gen");
export const { CredentialsCard } =
  await import("./components/providers/CredentialsCard");
export const { CredentialUsage } = await import("./components/CredentialUsage");
export const { ProviderCard } =
  await import("./components/providers/ProviderCard");
export const { ProvidersPage } = await import("./routes/models/providers");
export const { ProviderForm } =
  await import("./components/providers/ProviderForm");
export const { ModelCatalogPage } = await import("./routes/models/catalog");
export const { TooltipProvider } = await import("./components/ui/tooltip");
export const originalFetch = globalThis.fetch;

afterEach(() => {
  cleanup();
  globalThis.fetch = originalFetch;
  document.body.innerHTML = "";
});

export function mount(element: React.ReactNode) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return render(
    <QueryClientProvider client={client}>
      <TooltipProvider>{element}</TooltipProvider>
    </QueryClientProvider>,
  );
}

export const settings = {
  log_retention_days: 30,
  pricing_interval_hours: 24,
  discovery_interval_minutes: 60,
};
