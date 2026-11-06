import { queryOptions } from "@tanstack/react-query";
import * as api from "./generated/client";
import type { CatalogModel, CatalogProvider } from "./generated/schemas";

export type { CatalogModel };
export type Provider = CatalogProvider;
export function getModelCatalog(signal: AbortSignal) {
  return api.getModelCatalog({ signal });
}

export function modelCatalogQueryOptions() {
  return queryOptions({
    queryKey: ["model-catalog"],
    retry: false,
    queryFn: ({ signal }) => getModelCatalog(signal),
  });
}

export function refreshModelCatalog() {
  return api.refreshModelCatalog();
}
