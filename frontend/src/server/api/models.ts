import { queryOptions } from "@tanstack/react-query";
import * as api from "./generated/client";
import type {
  CatalogModel,
  CatalogProvider,
  ModelSettings,
} from "./generated/schemas";

export type { CatalogModel };
export type Provider = CatalogProvider;
export type Settings = ModelSettings;

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

export function modelSettingsQueryOptions() {
  return queryOptions({
    queryKey: ["model-settings"],
    retry: false,
    queryFn: ({ signal }) => getModelSettings(signal),
  });
}

export function refreshModelCatalog() {
  return api.refreshModelCatalog();
}

export function getModelSettings(signal: AbortSignal) {
  return api.getModelSettings({ signal });
}

export function saveModelSettings(value: Settings) {
  return api.updateModelSettings(value);
}

export function syncModelSettings() {
  return api.syncModelSettings();
}
