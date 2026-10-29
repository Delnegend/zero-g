import { createFileRoute } from "@tanstack/react-router";
import { ObservabilityPage } from "../App";

export const Route = createFileRoute("/llm-logs")({
  head: () => ({ meta: [{ title: "LLM Logs · Zero-G" }] }),
  component: () => <ObservabilityPage logsOnly />,
});
