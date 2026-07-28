/**
 * The window shell: a tab bar, the active view, and the settings save bar.
 *
 * `useSettings` owns the single draft every tab edits; the footer's Save is
 * the only writer. Saving routes through `updateSettings`, which re-registers
 * the hotkeys and returns their status — registration failures are summarised
 * here and detailed in the Hotkeys tab, never swallowed.
 */

import { useState } from "react";

import { useSettings } from "./hooks/useSettings";
import AudioTab from "./views/AudioTab";
import DictionaryTab from "./views/DictionaryTab";
import HotkeysTab from "./views/HotkeysTab";
import ProvidersTab from "./views/ProvidersTab";
import StatusView from "./views/StatusView";

const TABS = [
  { id: "status", label: "Status" },
  { id: "providers", label: "Providers" },
  { id: "hotkeys", label: "Hotkeys" },
  { id: "audio", label: "Audio" },
  { id: "dictionary", label: "Dictionary" },
] as const;

type TabId = (typeof TABS)[number]["id"];

export default function App() {
  const [tab, setTab] = useState<TabId>("status");
  const s = useSettings();

  return (
    <main className="panel">
      <header>
        <h1>Whisper Screen</h1>
      </header>

      <nav className="tabs">
        {TABS.map((t) => (
          <button
            key={t.id}
            className={tab === t.id ? "tab tab--active" : "tab"}
            onClick={() => setTab(t.id)}
          >
            {t.label}
          </button>
        ))}
      </nav>

      {s.error && <p className="error">{s.error}</p>}

      {!s.settings && !s.error && <p className="hint">Loading settings…</p>}

      {tab === "status" && <StatusView settings={s.settings} />}
      {s.settings && tab === "providers" && (
        <ProvidersTab settings={s.settings} onChange={s.change} />
      )}
      {s.settings && tab === "hotkeys" && (
        <HotkeysTab settings={s.settings} onChange={s.change} hotkeyStatus={s.hotkeyStatus} />
      )}
      {s.settings && tab === "audio" && <AudioTab settings={s.settings} onChange={s.change} />}
      {s.settings && tab === "dictionary" && (
        <DictionaryTab settings={s.settings} onChange={s.change} />
      )}

      {s.settings && tab !== "status" && (
        <footer className="savebar">
          <button onClick={s.save} disabled={!s.dirty || s.saving}>
            {s.saving ? "Saving…" : "Save"}
          </button>
          {s.dirty && !s.saving && <span className="muted">Unsaved changes</span>}
          {!s.dirty && s.hotkeyStatus && !s.saving && (
            <span className={s.hotkeyStatus.failed.length ? "over" : "ok"}>
              {s.hotkeyStatus.failed.length
                ? `Saved, but ${s.hotkeyStatus.failed.length} hotkey(s) failed — see the Hotkeys tab.`
                : "Saved."}
            </span>
          )}
        </footer>
      )}
    </main>
  );
}
