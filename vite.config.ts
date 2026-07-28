import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],

  // Tauri drives this dev server and hardcodes its URL in `tauri.conf.json`
  // (`devUrl`). `strictPort` makes a port collision fail loudly instead of
  // silently moving to 5174, which would leave the webview showing nothing.
  server: {
    port: 5173,
    strictPort: true,
  },

  // The Rust side logs to this terminal too; clearing it would eat the audio
  // device and hotkey diagnostics that matter most during development.
  clearScreen: false,

  // Vite hides env vars it doesn't recognise. Tauri sets these and the build
  // needs them to target the right platform.
  envPrefix: ["VITE_", "TAURI_ENV_"],
});
