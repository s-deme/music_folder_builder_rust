import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  build: {
    ssr: "src/ui.contract.fixture.tsx",
    outDir: ".test-dist",
    emptyOutDir: true,
    minify: false,
    rollupOptions: {
      output: {
        entryFileNames: "ui-contract.mjs",
      },
    },
  },
});
