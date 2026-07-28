/**
 * The typed boundary between React and Rust.
 *
 * These types are hand-mirrored from the `#[derive(Serialize)]` structs in
 * `src-tauri/src/{settings,commands,pipeline/events}.rs`. Rust serialises with
 * `rename_all = "camelCase"`, so the field names match one-for-one.
 *
 * Nothing else in the frontend calls `invoke` directly — if a command is not
 * wrapped here, it does not exist as far as the UI is concerned. That keeps the
 * IPC surface auditable in one file and means a signature change breaks the
 * build in one place instead of five.
 */

import { invoke } from "@tauri-apps/api/core";

export type Intent = "dictate" | "bridge" | "transform";

export type SttProvider = "groq" | "openAi" | "localWhisper";
export type CleanupProvider = "groq" | "openAi" | "anthropic" | "ollama" | "none";
export type WebTarget = "chatGpt" | "claude" | "gemini" | { custom: { url: string } };

export interface Hotkeys {
  dictate: string;
  bridge: string;
  transform: string;
  cancel: string;
}

export interface Settings {
  schemaVersion: number;
  hotkeys: Hotkeys;
  behavior: {
    tapToggleMs: number;
    soundCues: boolean;
    /** Ms the bridge waits for the web LLM tab to load before pasting. */
    bridgeDelayMs: number;
  };
  audio: {
    prewarm: boolean;
    prerollMs: number;
    maxUtteranceSecs: number;
    /** Input device name from `listInputDevices()`; "" means the OS default. */
    deviceName: string;
  };
  capture: { maxEdgePx: number };
  providers: {
    stt: SttProvider;
    cleanup: CleanupProvider;
    webTarget: WebTarget;
    sttModel: string;
    sttLanguage: string;
    /** Chat model for cleanup — must match the chosen `cleanup` provider. */
    cleanupModel: string;
    localModelPath: string;
  };
  dictionary: string[];
  snippets: { trigger: string; expansion: string }[];
}

export interface HotkeyStatus {
  registered: string[];
  failed: { accelerator: string; reason: string }[];
}

export interface RuntimeStatus {
  micOpen: boolean;
  micError: string | null;
  sampleRate: number | null;
  channels: number | null;
  activeSession: number | null;
  hotkeys: HotkeyStatus;
}

export interface CaptureInfo {
  width: number;
  height: number;
  sourceWidth: number;
  sourceHeight: number;
  monitor: string;
  elapsedMs: number;
  bytes: number;
}

/** Mirrors `secrets::Credential` — the three providers that take an API key. */
export type CredentialProvider = "groq" | "openAi" | "anthropic";

/**
 * All the frontend is ever allowed to know about a stored key. `hint` is a
 * masked rendering like `gsk_…9c1f`; key material never crosses IPC.
 */
export interface CredentialStatus {
  credential: CredentialProvider;
  label: string;
  present: boolean;
  /** True when the key came from an env var — render read-only. */
  fromEnv: boolean;
  hint: string | null;
}

export interface Timings {
  captureMs: number;
  recordMs: number;
  sttMs: number;
  finalizeMs: number;
  totalMs: number;
}

/**
 * Discriminated union matching `PipelineEvent` in Rust. The `type` tag comes
 * from `#[serde(tag = "type")]`, which lets TypeScript narrow each variant.
 */
export type PipelineEvent =
  | {
      type: "armed";
      sessionId: number;
      intent: Intent;
      prewarmed: boolean;
      capturingScreen: boolean;
    }
  | { type: "latched"; sessionId: number }
  | {
      type: "captureDone";
      sessionId: number;
      width: number;
      height: number;
      sourceWidth: number;
      sourceHeight: number;
      monitor: string;
      elapsedMs: number;
    }
  | { type: "recording"; sessionId: number; elapsedMs: number; level: number }
  | { type: "finalizing"; sessionId: number }
  | { type: "transcribed"; sessionId: number; text: string }
  | {
      type: "ready";
      sessionId: number;
      intent: Intent;
      /** Post-cleanup text; equals the raw transcript when cleanup is off or failed. */
      text: string;
      durationMs: number;
      sampleCount: number;
      peak: number;
      hasImage: boolean;
      truncated: boolean;
      timings: Timings;
    }
  | { type: "cancelled"; sessionId: number }
  | { type: "failed"; sessionId: number | null; message: string }
  | { type: "idle" };

/** Must match `PIPELINE_EVENT` in `src-tauri/src/pipeline/events.rs`. */
export const PIPELINE_EVENT = "pipeline://event";

export const getSettings = () => invoke<Settings>("get_settings");

export const updateSettings = (settings: Settings) =>
  invoke<HotkeyStatus>("update_settings", { settings });

export const runtimeStatus = () => invoke<RuntimeStatus>("runtime_status");

export const triggerSession = (intent: Intent) => invoke<void>("trigger_session", { intent });

export const stopSession = () => invoke<void>("stop_session");

export const cancelSession = () => invoke<void>("cancel_session");

export const testCapture = () => invoke<CaptureInfo>("test_capture");

/** Stores a key in the OS credential store; "" clears it. One-way: no command returns key material. */
export const setApiKey = (provider: CredentialProvider, key: string) =>
  invoke<void>("set_api_key", { provider, key });

export const clearApiKey = (provider: CredentialProvider) =>
  invoke<void>("clear_api_key", { provider });

export const apiKeyStatus = () => invoke<CredentialStatus[]>("api_key_status");

/** OS input device names for the microphone picker. A change applies on next app start. */
export const listInputDevices = () => invoke<string[]>("list_input_devices");
