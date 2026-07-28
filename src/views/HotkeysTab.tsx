/**
 * The four global accelerators, as plain text inputs.
 *
 * Saving re-registers all four in Rust and the registration is all-or-nothing
 * (PROJECT_NOTES §11), so the `HotkeyStatus` returned by `updateSettings` is
 * rendered here with the failure reason per accelerator rather than collapsed
 * into a toast. Deliberately no key-capture widget — a typed accelerator plus
 * an honest error beats a capture box that lies about what the OS rejected.
 */

import type { HotkeyStatus, Hotkeys, Settings } from "../lib/ipc";
import { TextField } from "./fields";

const ROWS = [
  ["dictate", "Dictate"],
  ["bridge", "Bridge + screenshot"],
  ["transform", "Transform selection"],
  ["cancel", "Cancel session"],
] as const satisfies readonly (readonly [keyof Hotkeys, string])[];

export default function HotkeysTab({
  settings,
  onChange,
  hotkeyStatus,
}: {
  settings: Settings;
  onChange: (next: Settings) => void;
  hotkeyStatus: HotkeyStatus | null;
}) {
  const setHotkey = (name: keyof Hotkeys, value: string) =>
    onChange({ ...settings, hotkeys: { ...settings.hotkeys, [name]: value } });

  const anyEmpty = ROWS.some(([name]) => settings.hotkeys[name].trim() === "");

  return (
    <>
      <section className="card">
        <h2>Accelerators</h2>
        {ROWS.map(([name, label]) => (
          <TextField
            key={name}
            label={label}
            value={settings.hotkeys[name]}
            mono
            placeholder="Alt+Shift+Space"
            onChange={(value) => setHotkey(name, value)}
          />
        ))}
        <p className="hint">
          Format like <kbd>Alt+Shift+Space</kbd> — modifiers <kbd>Alt</kbd>, <kbd>Shift</kbd>,{" "}
          <kbd>Ctrl</kbd>, <kbd>CmdOrControl</kbd>. Avoid bare <kbd>Escape</kbd>: a global binding
          would swallow the key from every app on the machine.
        </p>
        {anyEmpty && <p className="warn">An empty accelerator will fail to register.</p>}
      </section>

      {hotkeyStatus && (
        <section className="card">
          <h2>Registration result</h2>
          {hotkeyStatus.failed.length === 0 ? (
            <p className="hint">
              <span className="ok">All {hotkeyStatus.registered.length} accelerators bound.</span>{" "}
              Registration is all-or-nothing, so this is the whole story.
            </p>
          ) : (
            <>
              <p className="warn">
                Not bound — one conflict fails the whole batch, so all four are reported:
              </p>
              <ul className="keys">
                {hotkeyStatus.failed.map((f) => (
                  <li key={f.accelerator}>
                    <kbd>{f.accelerator}</kbd>
                    <span>{f.reason}</span>
                  </li>
                ))}
              </ul>
            </>
          )}
        </section>
      )}
    </>
  );
}
