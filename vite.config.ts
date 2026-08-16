import { defineConfig } from "vite";

// https://vite.dev/config/
export default defineConfig(() => ({
  // Vite options tailored for Tauri: the shell serves static splash/error
  // pages, so the build is a plain static copy.
  clearScreen: false,
  build: {
    outDir: "dist",
  },
}));
