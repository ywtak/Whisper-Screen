/**
 * Microphone, session behaviour, and screenshot capture settings.
 *
 * The device list comes from the OS via `listInputDevices()`; "" means the OS
 * default and is always offered. A device change is documented (in ipc.ts) as
 * applying on next app start, and the UI says so rather than implying a live
 * switch.
 */

import { useEffect, useState } from "react";

import { listInputDevices, type Settings } from "../lib/ipc";
import { CheckField, Field, NumberField } from "./fields";

export default function AudioTab({
  settings,
  onChange,
}: {
  settings: Settings;
  onChange: (next: Settings) => void;
}) {
  const [devices, setDevices] = useState<string[] | null>(null);
  const [deviceError, setDeviceError] = useState<string | null>(null);

  useEffect(() => {
    listInputDevices()
      .then(setDevices)
      .catch((err) => setDeviceError(String(err)));
  }, []);

  const a = settings.audio;
  const b = settings.behavior;
  const setAudio = (patch: Partial<Settings["audio"]>) =>
    onChange({ ...settings, audio: { ...a, ...patch } });
  const setBehavior = (patch: Partial<Settings["behavior"]>) =>
    onChange({ ...settings, behavior: { ...b, ...patch } });

  return (
    <>
      <section className="card">
        <h2>Microphone</h2>
        <Field label="Input device" hint="A device change applies on the next app start.">
          <select
            value={a.deviceName}
            onChange={(e) => setAudio({ deviceName: e.target.value })}
          >
            <option value="">OS default</option>
            {/* Keep a saved device visible even when it is currently unplugged. */}
            {a.deviceName !== "" && !devices?.includes(a.deviceName) && (
              <option value={a.deviceName}>{a.deviceName} (unavailable)</option>
            )}
            {devices?.map((name) => (
              <option key={name} value={name}>
                {name}
              </option>
            ))}
          </select>
        </Field>
        {deviceError && <p className="warn">Could not list devices: {deviceError}</p>}
        <CheckField
          label="Keep the microphone pre-warmed"
          checked={a.prewarm}
          hint="Costs a permanently lit OS mic indicator; without it the first take may clip."
          onChange={(prewarm) => setAudio({ prewarm })}
        />
        <NumberField
          label="Pre-roll (ms)"
          value={a.prerollMs}
          min={0}
          hint="Ring buffer that recovers the clipped first syllable."
          onChange={(prerollMs) => setAudio({ prerollMs })}
        />
        <NumberField
          label="Max utterance (s)"
          value={a.maxUtteranceSecs}
          min={5}
          max={600}
          hint="Bounds recording memory; Rust clamps this to 5–600."
          onChange={(maxUtteranceSecs) => setAudio({ maxUtteranceSecs })}
        />
      </section>

      <section className="card">
        <h2>Behavior</h2>
        <NumberField
          label="Tap-to-toggle (ms)"
          value={b.tapToggleMs}
          min={0}
          hint="Under this a tap latches recording on; over it the hotkey is push-to-talk."
          onChange={(tapToggleMs) => setBehavior({ tapToggleMs })}
        />
        <CheckField
          label="Sound cues"
          checked={b.soundCues}
          onChange={(soundCues) => setBehavior({ soundCues })}
        />
        <NumberField
          label="Bridge delay (ms)"
          value={b.bridgeDelayMs}
          min={0}
          hint="How long the bridge waits for the web LLM tab to load before pasting."
          onChange={(bridgeDelayMs) => setBehavior({ bridgeDelayMs })}
        />
      </section>

      <section className="card">
        <h2>Capture</h2>
        <NumberField
          label="Max screenshot edge (px)"
          value={settings.capture.maxEdgePx}
          min={0}
          hint="0 keeps native resolution; 1568 is Claude's limit."
          onChange={(maxEdgePx) =>
            onChange({ ...settings, capture: { ...settings.capture, maxEdgePx } })
          }
        />
      </section>
    </>
  );
}
