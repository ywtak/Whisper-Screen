/**
 * The Step-1 diagnostic panel, kept as one tab of the real UI.
 *
 * This is still where the pipeline proves itself: the live level meter, the
 * transcript, the per-stage latency breakdown, and the capture benchmark that
 * make the <50 ms capture and <800 ms round-trip budgets observable rather
 * than aspirational. It only reads settings (for the hotkey list) — editing
 * lives in the other tabs.
 */

import { useCallback, useEffect, useState } from "react";

import {
  cancelSession,
  runtimeStatus,
  stopSession,
  testCapture,
  triggerSession,
  type CaptureInfo,
  type RuntimeStatus,
  type Settings,
} from "../lib/ipc";
import { usePipeline } from "../hooks/usePipeline";

const PHASE_LABEL = {
  idle: "Idle",
  recording: "Recording",
  latched: "Recording (latched)",
  finalizing: "Processing",
} as const;

export default function StatusView({ settings }: { settings: Settings | null }) {
  const pipeline = usePipeline();
  const [status, setStatus] = useState<RuntimeStatus | null>(null);
  const [capture, setCapture] = useState<CaptureInfo | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refreshStatus = useCallback(() => {
    runtimeStatus()
      .then(setStatus)
      .catch((err) => setError(String(err)));
  }, []);

  useEffect(() => {
    refreshStatus();
  }, [refreshStatus]);

  // Re-read the mic/hotkey status whenever the pipeline changes phase — that is
  // when it can actually have changed, so there is no need for a poll timer.
  useEffect(refreshStatus, [pipeline.phase, refreshStatus]);

  const runCaptureTest = async () => {
    setError(null);
    try {
      setCapture(await testCapture());
    } catch (err) {
      setError(String(err));
    }
  };

  const busy = pipeline.phase !== "idle";

  return (
    <>
      <div className="statusline">
        <span>Pipeline</span>
        <span className={`badge badge--${pipeline.phase}`}>{PHASE_LABEL[pipeline.phase]}</span>
      </div>

      <section className="card">
        <div className="meter" role="meter" aria-valuenow={Math.round(pipeline.level * 100)}>
          {/* A square root makes quiet speech visible; raw RMS barely moves. */}
          <div
            className="meter__fill"
            style={{ width: `${Math.min(100, Math.sqrt(pipeline.level) * 140)}%` }}
          />
        </div>
        <div className="row row--mono">
          <span>{(pipeline.elapsedMs / 1000).toFixed(1)}s</span>
          <span>{pipeline.sessionId ? `session #${pipeline.sessionId}` : "no session"}</span>
        </div>

        {pipeline.coldStart && (
          <p className="warn">
            Mic opened on demand — the start of this take may be clipped. Enable pre-warm in the
            Audio tab.
          </p>
        )}

        <div className="row">
          <button onClick={() => triggerSession("dictate")} disabled={busy}>
            Dictate
          </button>
          <button onClick={() => triggerSession("bridge")} disabled={busy}>
            Bridge + shot
          </button>
          <button onClick={stopSession} disabled={!busy}>
            Stop
          </button>
          <button onClick={cancelSession} disabled={!busy}>
            Cancel
          </button>
        </div>
      </section>

      {pipeline.capture && (
        <section className="card">
          <h2>Screenshot leg</h2>
          <dl>
            <dt>Display</dt>
            <dd>{pipeline.capture.monitor}</dd>
            <dt>Size</dt>
            <dd>
              {pipeline.capture.width}×{pipeline.capture.height}
            </dd>
            <dt>Captured in</dt>
            <dd className={pipeline.capture.elapsedMs > 50 ? "over" : "ok"}>
              {pipeline.capture.elapsedMs} ms
            </dd>
          </dl>
        </section>
      )}

      {pipeline.transcript && (
        <section className="card">
          <h2>Transcript</h2>
          <p className="transcript">{pipeline.transcript}</p>
        </section>
      )}

      {pipeline.lastReady && (
        <section className="card">
          <h2>Last session</h2>
          <dl>
            <dt>Audio</dt>
            <dd>
              {(pipeline.lastReady.durationMs / 1000).toFixed(2)}s
              {pipeline.lastReady.truncated && " (truncated)"}
            </dd>
            <dt>Image</dt>
            <dd>{pipeline.lastReady.hasImage ? "yes" : "no"}</dd>
            <dt>Capture</dt>
            <dd>{pipeline.lastReady.timings.captureMs} ms</dd>
            <dt>STT</dt>
            <dd>{pipeline.lastReady.timings.sttMs} ms</dd>
            <dt>Finalize</dt>
            <dd>{pipeline.lastReady.timings.finalizeMs} ms</dd>
          </dl>
          <p className="hint">Step 4 attaches injection here; the text above is post-cleanup.</p>
        </section>
      )}

      <section className="card">
        <h2>Runtime</h2>
        <dl>
          <dt>Microphone</dt>
          <dd className={status?.micOpen ? "ok" : "over"}>
            {status?.micOpen ? "open" : "closed"}
            {status?.sampleRate ? ` · ${status.sampleRate} Hz · ${status.channels}ch` : ""}
          </dd>
          <dt>Pre-warm</dt>
          <dd>{settings ? (settings.audio.prewarm ? "on" : "off") : "—"}</dd>
        </dl>
        {status?.micError && <p className="warn">{status.micError}</p>}

        <h2>Hotkeys</h2>
        <ul className="keys">
          {settings &&
            (
              [
                ["Dictate", settings.hotkeys.dictate],
                ["Bridge", settings.hotkeys.bridge],
                ["Transform", settings.hotkeys.transform],
                ["Cancel", settings.hotkeys.cancel],
              ] as const
            ).map(([label, accelerator]) => (
              <li key={label}>
                <span>{label}</span>
                <kbd>{accelerator}</kbd>
              </li>
            ))}
        </ul>
        {status?.hotkeys.failed.length ? (
          <p className="warn">
            Not bound:{" "}
            {status.hotkeys.failed.map((f) => `${f.accelerator} (${f.reason})`).join(", ")}
          </p>
        ) : null}
      </section>

      <section className="card">
        <h2>Capture benchmark</h2>
        <button onClick={runCaptureTest}>Take one screenshot</button>
        {capture && (
          <p className="hint">
            {capture.sourceWidth}×{capture.sourceHeight} → {capture.width}×{capture.height} in{" "}
            <strong className={capture.elapsedMs > 50 ? "over" : "ok"}>
              {capture.elapsedMs} ms
            </strong>{" "}
            ({(capture.bytes / 1_048_576).toFixed(1)} MB in memory)
          </p>
        )}
      </section>

      {(error ?? pipeline.error) && <p className="error">{error ?? pipeline.error}</p>}
    </>
  );
}
