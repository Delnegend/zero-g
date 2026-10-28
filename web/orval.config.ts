import { defineConfig } from "orval";

export default defineConfig({
  settings: {
    input:
      process.env.ZERO_G_OPENAPI_INPUT ?? "./src/api/generated/openapi.json",
    output: {
      client: "zod",
      target:
        process.env.ZERO_G_CONTRACT_OUTPUT ?? "./src/api/generated/settings.ts",
      override: { zod: { version: 4 } },
    },
  },
});
