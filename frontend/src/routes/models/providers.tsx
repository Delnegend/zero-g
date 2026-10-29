import { createFileRoute } from "@tanstack/react-router";
import { Management } from "../../Management";

export const Route = createFileRoute("/models/providers")({
  head: () => ({ meta: [{ title: "Providers · Zero-G" }] }),
  component: () => <Management page="providers" />,
});
