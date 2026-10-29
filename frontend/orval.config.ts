import { defineConfig } from "orval";

const input =
  process.env.ZERO_G_OPENAPI_INPUT ?? "./src/server/api/generated/openapi.json";

export default defineConfig({
  client: {
    input: {
      target: input,
      override: {
        transformer: (spec) => ({
          ...spec,
          paths: Object.fromEntries(
            Object.entries(spec.paths ?? {}).filter(([path]) =>
              path.startsWith("/api/"),
            ),
          ),
        }),
      },
    },
    output: {
      client: "fetch",
      urlEncodeParameters: true,
      target: "./src/server/api/generated/client.ts",
      schemas: {
        path: "./src/server/api/generated/schemas.ts",
        mode: "single",
        type: "zod",
      },
      override: {
        mutator: { path: "./src/server/api/client.ts", name: "request" },
        fetch: {
          runtimeValidation: true,
          includeHttpResponseReturnType: false,
        },
        includeZodSchemaInArguments: true,
        zod: { version: 4 },
      },
    },
  },
});
