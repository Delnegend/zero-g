import { createFileRoute } from "@tanstack/react-router";
import { Management } from "../Management";

export const Route = createFileRoute("/settings")({
  head: () => ({ meta: [{ title: "Settings · Zero-G" }] }),
  component: () => <Management page="settings" />,
});
