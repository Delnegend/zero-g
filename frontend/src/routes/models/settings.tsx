import { createFileRoute } from "@tanstack/react-router";
import { ModelSettingsPage } from "../../Models";

export const Route = createFileRoute("/models/settings")({
  head: () => ({ meta: [{ title: "Model settings · Zero-G" }] }),
  component: ModelSettingsPage,
});
