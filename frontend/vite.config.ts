import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import http from "node:http";
import { readFileSync } from "node:fs";
import path from "node:path";
import { defineConfig, type Plugin } from "vite";

function dashboardAuth(): Plugin {
  return {
    name: "dashboard-auth",
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const url = new URL(req.url ?? "/", "http://localhost");
        const pathname = url.pathname;
        // These routes are already protected by the backend proxy.
        if (
          pathname === "/api" ||
          pathname.startsWith("/api/") ||
          ["/agy/", "/codex/", "/zen/", "/openai/", "/go/"].some((prefix) =>
            pathname.startsWith(prefix),
          ) ||
          pathname === "/auth/callback"
        ) {
          next();
          return;
        }

        const headers: http.OutgoingHttpHeaders = {};
        for (const name of ["host", "authorization", "cookie"] as const) {
          if (req.headers[name] !== undefined)
            headers[name] = req.headers[name];
        }
        const check = http.request(
          `http://127.0.0.1:8080${["/", "/llm-logs", "/settings", "/keys", "/models/catalog", "/models/providers", "/models/settings"].includes(pathname) ? pathname + url.search : "/"}`,
          {
            method: "GET",
            headers,
          },
          (upstream) => {
            if (upstream.statusCode === 200) {
              upstream.resume();
              next();
              return;
            }
            res.statusCode = upstream.statusCode ?? 502;
            for (const name of [
              "www-authenticate",
              "location",
              "set-cookie",
              "content-type",
              "cache-control",
            ] as const) {
              const value = upstream.headers[name];
              if (value !== undefined) res.setHeader(name, value);
            }
            upstream.pipe(res);
          },
        );
        check.on("error", () => {
          res.statusCode = 502;
          res.end("Dashboard backend unavailable");
        });
        check.end();
      });
    },
  };
}

function redocBundle(): Plugin {
  return {
    name: "redoc-bundle",
    generateBundle() {
      this.emitFile({
        type: "asset",
        fileName: "redoc.standalone.js",
        source: readFileSync(
          path.resolve(
            import.meta.dirname,
            "node_modules/redoc/bundles/redoc.standalone.js",
          ),
        ),
      });
    },
  };
}

// https://vite.dev/config/
export default defineConfig({
  plugins: [dashboardAuth(), redocBundle(), react(), tailwindcss()],
  server: {
    host: "127.0.0.1",
    port: 5173,
    strictPort: true,
    proxy: {
      "/agy": { target: "http://127.0.0.1:8080", changeOrigin: false },
      "/codex": { target: "http://127.0.0.1:8080", changeOrigin: false },
      "/zen": { target: "http://127.0.0.1:8080", changeOrigin: false },
      "/go": { target: "http://127.0.0.1:8080", changeOrigin: false },
      "/openai": { target: "http://127.0.0.1:8080", changeOrigin: false },
      "/redoc": { target: "http://127.0.0.1:8080", changeOrigin: false },
      "/api": { target: "http://127.0.0.1:8080", changeOrigin: false },
      "/auth/callback": {
        target: "http://127.0.0.1:8080",
        changeOrigin: false,
      },
    },
  },
  resolve: {
    alias: {
      "@": path.resolve(import.meta.dirname, "./src"),
    },
  },
  build: {
    chunkSizeWarningLimit: 2000,
  },
});
