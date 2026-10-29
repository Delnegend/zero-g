import { queryOptions } from "@tanstack/react-query";
import * as api from "./generated/client";
import type {
  AntigravityStartResponse,
  CodexStartResponse,
  Credential,
  CredentialInput,
  Provider as ProviderContract,
  ProviderKindsResponse,
  QuotaSnapshot,
  QuotaWindow as QuotaWindowContract,
} from "./generated/schemas";

export type ProviderKind = ProviderKindsResponse["kinds"][number];
export type Provider = ProviderContract;
export type ProviderCredential = Credential;
export type QuotaWindow = QuotaWindowContract;
export type CredentialUsage = QuotaSnapshot;
export type GoogleFlow = AntigravityStartResponse;
export type DeviceFlow = CodexStartResponse;
export type CredentialUpdate = CredentialInput;

export function saveProvider(
  id: string | undefined,
  body: Record<string, string>,
) {
  return id !== undefined
    ? api.updateProvider(id, body)
    : api.createProvider({ ...body, kind: body.kind });
}

export function deleteProvider(id: string) {
  return api.deleteProvider(id);
}

export function saveCredential(
  providerId: string,
  credentialId: string | undefined,
  body: CredentialUpdate,
) {
  return credentialId === undefined
    ? api.createCredential(providerId, body)
    : api.updateCredential(providerId, credentialId, body);
}

export function deleteCredential(providerId: string, credentialId: string) {
  return api.deleteCredential(providerId, credentialId);
}

export async function finishGoogleSignIn(
  providerId: string,
  credentialId: string | undefined,
  callbackUrl: string,
) {
  if (!credentialId) throw new Error("Credential is required");
  return api.finishAntigravityLogin(providerId, credentialId, {
    callback_url: callbackUrl,
  });
}

export async function pollSignIn(
  providerId: string,
  credentialId: string | undefined,
  kind: "agy" | "codex",
  id: string,
  signal: AbortSignal,
) {
  if (!credentialId) throw new Error("Credential is required");
  return kind === "agy"
    ? api.pollAntigravityLogin(providerId, credentialId, { id }, { signal })
    : api.pollCodexLogin(providerId, credentialId, { id }, { signal });
}

export function startGoogleSignIn(providerId: string, credentialId: string) {
  return api.startAntigravityLogin(providerId, credentialId);
}

export function startCodexSignIn(providerId: string, credentialId: string) {
  return api.startCodexLogin(providerId, credentialId);
}

export function getCredentialUsage(
  providerId: string,
  credentialId: string,
  signal?: AbortSignal,
) {
  return api.getCredentialUsage(providerId, credentialId, { signal });
}

export function saveConsoleSession(
  providerId: string,
  credentialId: string,
  body: Parameters<typeof api.saveZenConsole>[2],
) {
  return api.saveZenConsole(providerId, credentialId, body);
}

export function deleteConsoleSession(providerId: string, credentialId: string) {
  return api.deleteZenConsole(providerId, credentialId);
}

export function getCredentials(providerId: string, signal?: AbortSignal) {
  return api.listCredentials(providerId, { signal });
}

export function getProviderKinds(signal?: AbortSignal) {
  return api.listProviderKinds({ signal });
}

export function getProviders(signal?: AbortSignal) {
  return api.listProviders({ signal });
}

export function providersOptions() {
  return queryOptions({
    queryKey: ["providers"],
    retry: false,
    queryFn: ({ signal }) => getProviders(signal),
  });
}

export function providerKindsOptions() {
  return queryOptions({
    queryKey: ["provider-kinds"],
    retry: false,
    queryFn: ({ signal }) => getProviderKinds(signal),
  });
}

export function credentialsOptions(providerId: string) {
  return queryOptions({
    queryKey: ["credentials", providerId],
    queryFn: ({ signal }) => getCredentials(providerId, signal),
    retry: false,
  });
}

export function credentialUsageKey(providerId: string) {
  return ["credential-usage", providerId] as const;
}

export function credentialUsageOptions(
  providerId: string,
  credentialId: string,
  connected = true,
) {
  return queryOptions({
    queryKey: [...credentialUsageKey(providerId), credentialId],
    queryFn: ({ signal }) =>
      getCredentialUsage(providerId, credentialId, signal),
    enabled: connected,
    staleTime: 5 * 60 * 1000,
    refetchOnWindowFocus: false,
    retry: false,
  });
}
