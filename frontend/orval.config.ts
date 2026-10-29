import { defineConfig } from "orval";

export default defineConfig({
  settings: {
    input:
      process.env.ZERO_G_OPENAPI_INPUT ??
      "./src/server/api/generated/openapi.json",
    output: {
      client: "zod",
      target:
        process.env.ZERO_G_CONTRACT_OUTPUT ??
        "./src/server/api/generated/settings.ts",
      override: { zod: { version: 4 } },
    },
  },
});
