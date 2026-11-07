import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// The page lives under /admin: asset URLs start with it.
export default defineConfig({
  base: "/admin/",
  plugins: [react()],
  build: { outDir: "dist", emptyOutDir: true, target: "es2022", cssCodeSplit: false, assetsInlineLimit: 0 },
});
