/**
 * Owns the settings draft for the whole window.
 *
 * Rust owns `settings.json` exclusively (PROJECT_NOTES §7), so this hook is the
 * frontend half of that deal: load once, let every tab edit one local draft,
 * and push the whole object back through `updateSettings` on an explicit Save.
 * Explicit rather than debounced, because a half-typed accelerator is still a
 * real value — saving on every keystroke would churn hotkey re-registration
 * for nothing.
 */

import { useCallback, useEffect, useState } from "react";

import {
  getSettings,
  updateSettings,
  type HotkeyStatus,
  type Settings,
} from "../lib/ipc";

export interface SettingsState {
  /** The draft the tabs edit; null until the first load resolves. */
  settings: Settings | null;
  dirty: boolean;
  saving: boolean;
  /** Load or save failure, rendered by the shell. */
  error: string | null;
  /** Result of the last save — hotkey (re)registration failures land here. */
  hotkeyStatus: HotkeyStatus | null;
  change: (next: Settings) => void;
  save: () => Promise<void>;
}

export function useSettings(): SettingsState {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [hotkeyStatus, setHotkeyStatus] = useState<HotkeyStatus | null>(null);

  useEffect(() => {
    getSettings()
      .then((loaded) => {
        setSettings(loaded);
        setError(null);
      })
      .catch((err) => setError(String(err)));
  }, []);

  const change = useCallback((next: Settings) => {
    setSettings(next);
    setDirty(true);
  }, []);

  const save = useCallback(async () => {
    if (!settings || saving) return;
    setSaving(true);
    setError(null);
    try {
      const status = await updateSettings(settings);
      setHotkeyStatus(status);
      // Rust clamps out-of-range values on update; pull the sanitised object
      // back so the draft shows what is actually stored, not what was typed.
      setSettings(await getSettings());
      setDirty(false);
    } catch (err) {
      setError(String(err));
    } finally {
      setSaving(false);
    }
  }, [settings, saving]);

  return { settings, dirty, saving, error, hotkeyStatus, change, save };
}
