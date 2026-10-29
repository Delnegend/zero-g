import { queryOptions } from "@tanstack/react-query";
import * as api from "./generated/client";
import type {
  CatalogProvider,
  ModelCatalog,
  ModelData,
  ModelSettings,
} from "./generated/schemas";

type ModelValue = CatalogProvider["models"][number];
export type Model = ModelValue & Record<string, unknown> & { id: string };
export type Provider = Omit<CatalogProvider, "models"> & { models: Model[] };
export type Catalog = Omit<ModelCatalog, "providers"> & {
  providers: Provider[];
};
export type Datasheets = ModelData;
export type Settings = ModelSettings;

function hasModelId(value: ModelValue): value is Model {
  return (
    typeof value === "object" &&
    value !== null &&
    "id" in value &&
    typeof value.id === "string" &&
    value.id.length > 0
  );
}

function catalogForDashboard(catalog: ModelCatalog): Catalog {
  return {
    ...catalog,
    providers: catalog.providers.map((provider) => ({
      ...provider,
      models: provider.models.filter(hasModelId),
    })),
  };
}

export function getModelCatalog(signal: AbortSignal) {
  return api.getModelCatalog({ signal }).then(catalogForDashboard);
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
  return api.refreshModelCatalog().then(catalogForDashboard);
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
