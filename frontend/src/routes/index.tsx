import { createFileRoute } from "@tanstack/react-router";
import { ObservabilityPage } from "../App";

export const Route = createFileRoute("/")({
  head: () => ({ meta: [{ title: "Dashboard · Zero-G" }] }),
  component: ObservabilityPage,
});
