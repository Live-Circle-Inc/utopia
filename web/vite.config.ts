import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

export default defineConfig({
  plugins: [react(), tailwindcss()],
  server: {
    // Parallel development across several sessions: PORT is handed out by the launcher (another
    // port when 5173 is taken); keep the default when it is unset
    port: process.env.PORT ? Number(process.env.PORT) : 5173,
    proxy: {
      // The backend port can be overridden with UTOPIA_DEV_API (default 1516, matching
      // UTOPIA_BIND_ADDR in .env)
      "/api": process.env.UTOPIA_DEV_API ?? "http://127.0.0.1:1516",
    },
  },
});
