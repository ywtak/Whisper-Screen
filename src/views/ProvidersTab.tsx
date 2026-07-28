/**
 * Provider selection and API keys.
 *
 * The STT picker *is* the local-vs-cloud toggle (Groq/OpenAI vs. an offline
 * whisper.cpp build), and the keys section is deliberately one-way: the IPC
 * surface never returns key material, so the UI renders only the masked hint
 * the backend provides and never claims to show an existing key.
 */

import { useCallback, useEffect, useState } from "react";

import {
  apiKeyStatus,
  clearApiKey,
  setApiKey,
  type CleanupProvider,
  type CredentialStatus,
  type Settings,
  type SttProvider,
} from "../lib/ipc";
import { Field, TextField } from "./fields";

interface TabProps {
  settings: Settings;
  onChange: (next: Settings) => void;
}

/** Shown as placeholders only — the stored value stays whatever the user set. */
const STT_MODEL_PLACEHOLDER: Record<SttProvider, string> = {
  groq: "whisper-large-v3-turbo",
  openAi: "whisper-1",
  localWhisper: "ggml-base.en.bin",
};

const CLEANUP_MODEL_PLACEHOLDER: Record<CleanupProvider, string> = {
  groq: "llama-3.3-70b-versatile",
  openAi: "gpt-4o-mini",
  anthropic: "claude-haiku-4-5",
  ollama: "llama3.2",
  none: "",
};

export default function ProvidersTab({ settings, onChange }: TabProps) {
  const p = settings.providers;
  const setProviders = (patch: Partial<Settings["providers"]>) =>
    onChange({ ...settings, providers: { ...p, ...patch } });

  const targetKind = typeof p.webTarget === "string" ? p.webTarget : "custom";
  const customUrl = typeof p.webTarget === "string" ? "" : p.webTarget.custom.url;

  return (
    <>
      <section className="card">
        <h2>Speech to text</h2>
        <Field label="Provider" hint="Local Whisper runs offline; the cloud providers need a key below.">
          <select
            value={p.stt}
            onChange={(e) => setProviders({ stt: e.target.value as SttProvider })}
          >
            <option value="groq">Groq (cloud)</option>
            <option value="openAi">OpenAI (cloud)</option>
            <option value="localWhisper">Local Whisper (offline)</option>
          </select>
        </Field>
        <TextField
          label="Model"
          value={p.sttModel}
          placeholder={STT_MODEL_PLACEHOLDER[p.stt]}
          mono
          onChange={(sttModel) => setProviders({ sttModel })}
        />
        <TextField
          label="Language"
          value={p.sttLanguage}
          placeholder="en"
          hint="ISO code sent to the STT provider."
          onChange={(sttLanguage) => setProviders({ sttLanguage })}
        />
        {p.stt === "localWhisper" && (
          <TextField
            label="Model file"
            value={p.localModelPath}
            placeholder="Path to a ggml .bin"
            mono
            onChange={(localModelPath) => setProviders({ localModelPath })}
          />
        )}
      </section>

      <section className="card">
        <h2>Cleanup</h2>
        <Field
          label="Provider"
          hint="Polishes the raw transcript. “None” skips the pass — faster, and the fallback when a provider errors."
        >
          <select
            value={p.cleanup}
            onChange={(e) => setProviders({ cleanup: e.target.value as CleanupProvider })}
          >
            <option value="groq">Groq</option>
            <option value="openAi">OpenAI</option>
            <option value="anthropic">Anthropic</option>
            <option value="ollama">Ollama (local)</option>
            <option value="none">None</option>
          </select>
        </Field>
        {p.cleanup !== "none" && (
          <TextField
            label="Model"
            value={p.cleanupModel}
            placeholder={CLEANUP_MODEL_PLACEHOLDER[p.cleanup]}
            mono
            onChange={(cleanupModel) => setProviders({ cleanupModel })}
          />
        )}
      </section>

      <section className="card">
        <h2>Web bridge</h2>
        <Field label="Target" hint="Where a bridge session pastes the screenshot and your question.">
          <select
            value={targetKind}
            onChange={(e) => {
              const kind = e.target.value;
              setProviders({
                webTarget:
                  kind === "custom"
                    ? { custom: { url: customUrl } }
                    : (kind as "chatGpt" | "claude" | "gemini"),
              });
            }}
          >
            <option value="chatGpt">ChatGPT</option>
            <option value="claude">Claude</option>
            <option value="gemini">Gemini</option>
            <option value="custom">Custom URL…</option>
          </select>
        </Field>
        {targetKind === "custom" && (
          <TextField
            label="URL"
            value={customUrl}
            placeholder="https://…"
            mono
            onChange={(url) => setProviders({ webTarget: { custom: { url } } })}
          />
        )}
      </section>

      <KeysCard />
    </>
  );
}

function KeysCard() {
  const [creds, setCreds] = useState<CredentialStatus[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(() => {
    apiKeyStatus()
      .then(setCreds)
      .catch((err) => setError(String(err)));
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  return (
    <section className="card">
      <h2>API keys</h2>
      <p className="hint">
        Keys live in the OS credential store and are write-only over IPC — only the masked hint is
        ever shown, never the key itself.
      </p>
      {error && <p className="error">{error}</p>}
      {creds?.map((cred) => (
        <KeyRow key={cred.credential} cred={cred} onChanged={refresh} />
      ))}
      {creds && creds.length === 0 && <p className="hint">No credentials configured.</p>}
    </section>
  );
}

function KeyRow({ cred, onChanged }: { cred: CredentialStatus; onChanged: () => void }) {
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    try {
      await action();
      setDraft("");
      onChanged();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="keyrow">
      <div className="keyrow__head">
        <span>{cred.label}</span>
        {cred.fromEnv ? (
          <span className="badge">from environment</span>
        ) : cred.present ? (
          <span className="badge badge--ok">set</span>
        ) : (
          <span className="badge">not set</span>
        )}
      </div>
      {cred.hint && <kbd>{cred.hint}</kbd>}
      {cred.fromEnv ? (
        <p className="hint">Set via environment variable, which wins over the store.</p>
      ) : (
        <>
          <div className="addrow">
            <input
              type="password"
              value={draft}
              placeholder={cred.present ? "Paste a new key to replace" : "Paste a new key"}
              autoComplete="off"
              onChange={(e) => setDraft(e.target.value)}
            />
            <button
              disabled={busy || draft.trim() === ""}
              onClick={() => run(() => setApiKey(cred.credential, draft.trim()))}
            >
              Save
            </button>
            <button
              disabled={busy || !cred.present}
              onClick={() => run(() => clearApiKey(cred.credential))}
            >
              Clear
            </button>
          </div>
          {error && <p className="warn">{error}</p>}
        </>
      )}
    </div>
  );
}
