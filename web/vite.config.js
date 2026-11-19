import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// `npm run dev` serves the UI with hot reload and forwards /api to a running `agent --web`
// (AGENT_URL, default http://127.0.0.1:8080). The release build is plain static files in
// dist/, which the Rust binary embeds and serves.
export default defineConfig({
  plugins: [svelte()],
  server: {
    proxy: { "/api": process.env.AGENT_URL || "http://127.0.0.1:8080" },
  },
});
