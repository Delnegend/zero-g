import { createFileRoute } from "@tanstack/react-router";
import { ModelCatalogPage } from "../../Models";

export const Route = createFileRoute("/models/catalog")({
  head: () => ({ meta: [{ title: "Catalog · Zero-G" }] }),
  component: ModelCatalogPage,
});
