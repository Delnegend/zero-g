import type { z } from "zod";

export async function request<T>(
  url: string,
  options?: RequestInit & {
    schema?: z.ZodType;
    invalidMessage?: string;
    statusOnlyError?: boolean;
  },
): Promise<T> {
  const { schema, invalidMessage, statusOnlyError, ...init } = options ?? {};
  const response = await fetch(url, init);
  if (!response.ok) {
    if (statusOnlyError) {
      throw new Error(`Request failed (${response.status})`);
    }
    const message = await response.text();
    throw new Error(message || `Request failed (${response.status})`);
  }
  if (response.status === 204) return undefined as T;
  const data: unknown = await response.json();
  if (!schema) return data as T;
  const result = schema.safeParse(data);
  if (!result.success) {
    throw new Error(invalidMessage ?? "Invalid response from server");
  }
  return result.data as T;
}

export function message(error: unknown): string {
  return error instanceof Error ? error.message : "Request failed";
}
