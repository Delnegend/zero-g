import { queryOptions } from "@tanstack/react-query";
import * as api from "./generated/client";
import type { CreateKey, KeyInfo, UpdateKey } from "./generated/schemas";

export type VirtualKey = KeyInfo;

export function getKeys(signal?: AbortSignal) {
  return api.listKeys({ signal });
}

export function keysOptions() {
  return queryOptions({
    queryKey: ["virtual-keys"],
    retry: false,
    queryFn: ({ signal }) => getKeys(signal),
  });
}

export function createKey(body: CreateKey) {
  return api.createKey(body);
}

export function revokeKey(id: string) {
  return api.revokeKey(id);
}

export function updateKey(id: string, body: UpdateKey) {
  return api.updateKey(id, body);
}
