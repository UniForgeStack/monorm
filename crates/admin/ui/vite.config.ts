import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// Asset URLs are relative: the server writes the base path of the site before each one when it serves the index.
export default defineConfig({
  base: "./",
  plugins: [react()],
  build: { outDir: "dist", emptyOutDir: true, target: "es2022", cssCodeSplit: false, assetsInlineLimit: 0 },
});
