# Architecture

## 1. The parallel execution pipeline

### The two legs are not the same kind of work

The instinct is to spawn two async tasks and `join!` them. That is wrong here,
because the two legs have incompatible natures:

| | Screen capture | Audio capture |
|---|---|---|
| Shape | One-shot, **blocking** syscall (DXGI Desktop Duplication / `CGDisplayCreateImage`) | **Continuous** stream with a realtime callback |
| Duration | 15–40 ms | Seconds — as long as the user talks |
| Thread safety | `Send`, no state | `cpal::Stream` is **`!Send` on macOS**, and dropping it blocks |
| Right home | `spawn_blocking` (Tokio's warm blocking pool) | A dedicated OS thread that owns the device for the process lifetime |

Putting the DXGI call on an async worker would stall every other task on that
worker. Holding a `cpal::Stream` across an `.await` will not compile on macOS
and, where it does compile, risks blocking a runtime thread on device teardown.
So:

- **Screenshot** → `tauri::async_runtime::spawn_blocking`, one task per session.
- **Audio** → one long-lived thread (`audio::engine`), created at startup, never
  handed a session-shaped lifetime at all.

### The single-owner state machine

```
  OS hotkey callback (win32 WndProc / macOS CFRunLoop)
        │  tx.send(Trigger)      <- non-blocking, no locks, no I/O, ~200 ns
        v
  supervisor task -- owns `Option<Active>` as a PLAIN LOCAL VARIABLE
        │
        ├── leg 1: audio.start_session(id)   -> AudioEngine thread (pre-warmed)
        └── leg 2: spawn_blocking(capture)   -> runs while the user is still talking
        │
        v  (on key-up)
  detached finalize task -> join!(capture, resample) -> PipelineEvent::Ready
```

The in-flight session lives in a local `Option<Active>` inside one task
(`pipeline::spawn`). Exactly one task ever reads or writes it. That is the whole
race-condition strategy: **there is no shared mutable session state to race on.**
No `Mutex<Option<Session>>`, therefore no chance of holding a lock across an
`.await` — the standard way async Rust deadlocks itself.

### Where the concurrency actually lives

The screenshot is spawned at **key-down**, not key-up. By the time the user
releases the key, it finished seconds ago. So `tokio::join!` in `finalize` is not
where the parallelism happens — it is just where two already-running legs are
collected. Spawning capture at key-up instead would serialise its full cost onto
the visible latency. This distinction is the point of the design.

### Race conditions: how each one is eliminated

| Race | Mitigation |
|---|---|
| Two overlapping sessions | Monotonic `SessionId`; every stage tags its output, and `CaptureBuffer::finish(id)` returns `None` for a stale id rather than handing over the wrong recording |
| Key autorepeat re-arming a live session | `CaptureBuffer::begin` is idempotent per id |
| Stale `Recording` tick updating the UI after a new session started | Reducer drops events whose `sessionId` differs from the current one |
| Hotkey callback contending with the audio callback | The callback only does `tx.send`; it never touches the buffer |
| Supervisor blocked during a slow finalize (STT in Step 2) | Finalize is **detached** onto its own task |

### Memory leaks: how each one is eliminated

- **The device is never owned by a session.** One thread holds it for the process
  lifetime, so no session can leak it.
- **Buffers are capacity-capped.** `max_utterance_secs` bounds the session vector;
  on overflow it stops appending and sets a flag instead of growing.
- **`Active::drop`** cancels its `CancellationToken` and aborts the capture
  handle, so dropping a session releases the screenshot's memory even if nobody
  awaited it.
- **Screenshots are downscaled before they are stored** (1568 px long edge), so a
  queued frame is ~4 MB, not 33 MB.
- **Honest limit:** `spawn_blocking` work cannot actually be interrupted. A DXGI
  call already in flight runs to completion; we discard its result via the
  session-id check. `abort()` releases the handle, not the thread.

### Hitting the latency budget

| Trick | Saving |
|---|---|
| **Pre-warm the mic** — device open at startup, `start_session` is one mutex | 20–200 ms of WASAPI/CoreAudio device open, per session |
| **Pre-roll ring** — 400 ms of audio from *before* the keypress is prepended | Recovers the clipped first syllable; this is why good dictation apps feel like they started listening early |
| **Downmix in the callback** | Buffer shrinks by `channels`x; resample cost drops proportionally |
| **Defer PNG encoding** | ~300 ms at 4K. The clipboard path wants raw RGBA anyway, so the common case never encodes |
| **Downscale before encode** | Turns a 300 ms encode into ~20 ms when encoding is needed |
| **Intent known at t=0** | Dictation skips the screenshot entirely — the hotkey *is* the intent |
| **`opt-level = 1` in dev** | The per-frame downmix risks dropping samples at `opt-level = 0` |

Pre-warm's cost is a permanently lit OS mic indicator. That is the user's call,
not ours: `settings.audio.prewarm` controls it, defaulting to on.

### Push-to-talk and tap-to-toggle from one binding

`ShortcutState::{Pressed, Released}` gives key-down and key-up. Hold longer than
`tap_toggle_ms` (300 ms) → push-to-talk, ends on release. Tap shorter → latches
on, and the next press stops it. One binding covers both a two-word correction
and a two-minute monologue.

---

## 2. Directory structure

Files marked `*` exist now; the rest are the planned homes for Modules 1–4.

```
Whisper-Screen/
├─ ARCHITECTURE.md                  * this file
├─ README.md                        * prerequisites and how to run
├─ package.json  vite.config.ts     * tsconfig.*.json  index.html
│
├─ src/                             # React tray panel
│  ├─ main.tsx                      * entry
│  ├─ App.tsx                       * shell: tab bar + save bar; owns the settings draft
│  ├─ styles.css                    * single stylesheet, follows OS theme
│  ├─ lib/
│  │  └─ ipc.ts                     * THE typed IPC boundary; nothing else invokes
│  ├─ hooks/
│  │  ├─ usePipeline.ts             * folds pipeline events into view state
│  │  └─ useSettings.ts             * settings draft + save via update_settings
│  └─ views/                          StatusView, ProvidersTab (+API keys), HotkeysTab,
│                                     AudioTab, DictionaryTab, shared fields
│
└─ src-tauri/
   ├─ Cargo.toml  build.rs          *
   ├─ tauri.conf.json               * hidden window, tray-style, real CSP
   ├─ capabilities/default.json     * IPC permission grants
   ├─ icons/                        *
   └─ src/
      ├─ main.rs                    * 3-line entry
      ├─ lib.rs                     * wiring: plugins, state, tray, setup
      ├─ error.rs                   * one AppError, Serialize for IPC
      ├─ settings.rs                * settings.json, atomic writes, sanitize()
      ├─ commands.rs                * IPC surface (7 commands)
      ├─ hotkey.rs                  * registrar; the "do no work" callback
      │
      ├─ pipeline/
      │  ├─ mod.rs                  * THE SUPERVISOR — start reading here
      │  ├─ session.rs              * Intent, Trigger, SessionIds, Active
      │  └─ events.rs               * PipelineEvent -> frontend
      │
      ├─ audio/
      │  ├─ mod.rs                  * WAV encoding
      │  ├─ engine.rs               * the thread that owns the mic
      │  ├─ buffer.rs               * pre-roll ring + bounded session buffer
      │  └─ resample.rs             * device rate -> 16 kHz mono
      │
      ├─ capture/
      │  ├─ mod.rs                  *
      │  ├─ screen.rs               * silent, downscaled, blocking
      │  ├─ selection.rs              Module 3: read the highlighted text
      │  └─ window.rs                 window-scoped capture
      │
      ├─ stt/                         Step 2: groq.rs, openai.rs, local.rs
      ├─ llm/                         Step 3: cleanup prompts, provider clients
      ├─ inject/                      Step 4: enigo typing, clipboard, focus
      └─ bridge/                      Step 5: web-LLM targeting + paste
```

---

## 3. Build order after Step 1

| Step | Adds | New deps |
|---|---|---|
| 2 | `stt/` — Groq/OpenAI Whisper over `reqwest`, `whisper.cpp` for offline | `reqwest`, `whisper-rs` |
| 3 | `llm/` — filler-word stripping, backtracking, camelCase/CLI awareness, dictionary bias, snippet expansion | — |
| 4 | `inject/` — active-cursor typing, clipboard save/restore, focus checks | `enigo`, `arboard` |
| 5 | `bridge/` — open/focus the target web LLM, paste image + prompt | `tauri-plugin-opener` (already in) |
| 6 | `capture/selection.rs` — Module 3 in-place transform | — |
| 7 | Full settings UI, API-key storage in the OS keychain | `keyring` |

### Known gaps to close before this is a product

- **API keys** must go to the OS keychain (`keyring` crate), never to
  `settings.json`. Step 2 territory, but decide it before writing the STT client.
- **Bare `Escape` to cancel** requires registering it only while a session is
  live — a global `Escape` binding would swallow the key from every other app.
  Default is `Alt+Escape` until then.
- **macOS permissions**: screen recording, microphone, and Accessibility (for
  Step 4's synthetic keystrokes) each need `Info.plist` usage strings and a
  first-run onboarding flow. Accessibility in particular cannot be requested
  silently.
- **`cpal` callback locking**: `parking_lot::Mutex` held for ~480 samples is fine
  in practice but is not hard-realtime. Swap in an SPSC ring (`ringbuf`) if you
  ever see dropouts under load; `CaptureBuffer`'s public API will not change.
