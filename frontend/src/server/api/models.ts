import { queryOptions } from "@tanstack/react-query";
import * as api from "./generated/client";
import type {
  CatalogProvider,
  Model,
  ModelData,
  ModelSettings,
} from "./generated/schemas";

export type { Model };
export type Provider = CatalogProvider;
export type Datasheets = ModelData;
export type Settings = ModelSettings;

export function getModelCatalog(signal: AbortSignal) {
  return api.getModelCatalog({ signal });
}

export function getModelDatasheets(signal: AbortSignal) {
  return api.getModelData({ signal });
}

export function modelCatalogQueryOptions() {
  return queryOptions({
    queryKey: ["model-catalog"],
    retry: false,
    queryFn: ({ signal }) => getModelCatalog(signal),
  });
}

export function modelDatasheetsQueryOptions() {
  return queryOptions({
    queryKey: ["model-settings-data"],
    retry: false,
    staleTime: 30_000,
    queryFn: ({ signal }) => getModelDatasheets(signal),
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
