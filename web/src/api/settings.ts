import { z } from "zod";
import {
  GetSettingsResponse,
  UpdateSettingsBody,
  UpdateSettingsResponse,
} from "./generated/settings";

export type Settings = z.output<typeof GetSettingsResponse>;
export type SettingsUpdate = z.input<typeof UpdateSettingsBody>;

async function settingsRequest<T>(
  schema: z.ZodType<T>,
  options: RequestInit,
): Promise<T> {
  const response = await fetch("/api/settings", options);
  if (!response.ok) {
    const message = await response.text();
    throw new Error(message || `Request failed (${response.status})`);
  }
  const result = schema.safeParse(await response.json());
  if (!result.success) throw new Error("Invalid settings response from server");
  return result.data;
}

export function getSettings(signal: AbortSignal) {
  return settingsRequest(GetSettingsResponse, { signal });
}

export function updateSettings(body: SettingsUpdate, signal: AbortSignal) {
  return settingsRequest(UpdateSettingsResponse, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
    signal,
  });
}
