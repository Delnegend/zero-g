import { createFileRoute } from "@tanstack/react-router";
import { Management } from "../Management";

export const Route = createFileRoute("/keys")({
  head: () => ({ meta: [{ title: "Virtual keys · Zero-G" }] }),
  component: () => <Management page="keys" />,
});
