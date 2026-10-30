import type {
  DeviceFlow,
  GoogleFlow,
  ProviderCredential,
} from "@/server/api/providers";

export type SignInFlow =
  | { kind: "google"; credential: ProviderCredential; data: GoogleFlow }
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

export type CallbackFlow = Extract<SignInFlow, { kind: "google" | "claude" }>;
export type PollingFlow = Exclude<SignInFlow, { kind: "claude" }>;
