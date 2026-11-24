import type { DeviceFlow, ProviderCredential } from "@/server/api/providers";

export type SignInFlow =
  | { kind: "device"; credential: ProviderCredential; data: DeviceFlow }
  | {
      kind: "claude";
      credential: ProviderCredential;
      data: { id: string; url: string };
    }
  | {
      kind: "cursor";
      credential: ProviderCredential;
      data: { id: string; url: string; interval: number };
    };

export type CallbackFlow = Extract<SignInFlow, { kind: "claude" }>;
export type PollingFlow = Exclude<SignInFlow, { kind: "claude" }>;
