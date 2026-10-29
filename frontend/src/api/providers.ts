import { request } from "./client";

export type ProviderKind = {
  value: string;
  label: string;
  oauth: boolean;
  api_key: boolean;
  fixed_id: string | null;
  fixed_name: string | null;
};

export type Provider = {
  id: string;
  name: string;
  kind: string;
  base_url: string;
};

export type ProviderCredential = {
  id: string;
  name: string;
  weight: number;
  enabled: boolean;
  connected: boolean;
};

export type QuotaWindow = {
  label: string;
  remaining_percent: number | null;
  reset_at: number | null;
};

export type CredentialUsage =
  | { kind: "quota"; windows: QuotaWindow[] }
  | { kind: "balance"; amount: number; currency: string };

export function getCredentialUsage(
  providerId: string,
  credentialId: string,
  signal?: AbortSignal,
) {
  return request<CredentialUsage>(
    `${credentialsPath(providerId, credentialId)}/usage`,
    { signal },
  );
}

export function saveConsoleSession(
  providerId: string,
  credentialId: string,
  body: { workspace_id: string; session: string },
) {
  return request<void>(`${credentialsPath(providerId, credentialId)}/console`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
}

export function deleteConsoleSession(providerId: string, credentialId: string) {
  return request<void>(`${credentialsPath(providerId, credentialId)}/console`, {
    method: "DELETE",
  });
}

export function credentialsPath(providerId: string, credentialId?: string) {
  const path = `${providerPath(providerId)}/credentials`;
  return credentialId ? `${path}/${encodeURIComponent(credentialId)}` : path;
}

export function getCredentials(providerId: string, signal?: AbortSignal) {
  return request<{ credentials: ProviderCredential[] }>(
    credentialsPath(providerId),
    { signal },
  );
}

export function getProviderKinds(signal?: AbortSignal) {
  return request<{ kinds: ProviderKind[] }>("/api/provider-kinds", { signal });
}

export function getProviders(signal?: AbortSignal) {
  return request<{ providers: Provider[] }>("/api/providers", { signal });
}

export function providerPath(id: string) {
  return `/api/providers/${encodeURIComponent(id)}`;
}
