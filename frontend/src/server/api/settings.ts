import { queryOptions } from "@tanstack/react-query";
import * as api from "./generated/client";
import type { SettingsResponse, UpdateSettings } from "./generated/schemas";

export type Settings = SettingsResponse;
export type SettingsUpdate = UpdateSettings;

export function getSettings(signal: AbortSignal) {
  return api.getSettings({
    signal,
    invalidMessage: "Invalid settings response from server",
  });
}

export function settingsOptions() {
  return queryOptions({
    queryKey: ["settings"],
    retry: false,
    queryFn: ({ signal }) => getSettings(signal),
  });
}

export function updateSettings(body: SettingsUpdate, signal: AbortSignal) {
  return api.updateSettings(body, {
    signal,
    invalidMessage: "Invalid settings response from server",
  });
}
