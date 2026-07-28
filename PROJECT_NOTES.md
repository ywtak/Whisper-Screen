# Whisper Screen — Project Reference

Living context for anyone (human or agent) picking this repo up cold.
Written 2026-07-28, verified against the working tree on that date.

`README.md` and `ARCHITECTURE.md` are still worth reading — the architecture
rationale in particular is good and still accurate. **But both have drifted from
the code.** This file is the ground truth where they disagree.

---

## 1. Read this first

| | |
|---|---|
| **What it is** | Tray-resident voice utility. One hotkey opens the mic *and* grabs the screen simultaneously; the result routes either into the active text cursor or into a web LLM alongside the screenshot. Inspired by Wispr Flow. |
| **Stack** | Tauri 2.11 · Rust 2021 (MSRV 1.82) · React 19 · TypeScript 6 · Vite 8 |
| **Frontend** | ✅ Builds clean. `npm run build` and `npm run lint` both pass. Full settings UI (Status / Providers+Keys / Hotkeys / Audio / Dictionary tabs). |
| **Backend** | ✅ Compiles clean (cargo build + clippy `--all-targets`). All modules wired: stt, llm, inject, bridge, capture::selection, secrets (keychain). |
| **Rust tests** | ✅ **83 pass** (`cargo test --lib`, 2026-07-28). Caveat: Windows Smart App Control intermittently blocks freshly built test binaries (os error 4551) — if a run is blocked, touch a source file to change the hash and retry. |
| **App** | ❌ Has never been launched. `cargo build` succeeds, but no runtime behaviour (tray, hotkeys, injection, bridge timing) has been observed on hardware. |
| **Git** | ⚠️ `git init` done 2026-07-28, but no commits yet. |

---

## 2. Where the existing docs are wrong

| Doc claims | Reality |
|---|---|
| README: "The Rust toolchain is not installed on this machine" | It **is**. `rustc` / `cargo` 1.97.1 are installed and working. Ignore the install instructions. |
| README: "Status: Step 1 complete" | Step 1 code is written but has **never compiled**. Treat it as unverified. |
| README: "Verify Step 1 → `cargo test`" | This has never passed. It fails to build. |
| ARCHITECTURE §2: `stt/`, `llm/` listed as *planned* | They **exist on disk** with substantial code. But they are not wired into the build. See §4. |
| ARCHITECTURE §3: Step 2 "new deps: `reqwest`, `whisper-rs`" | `reqwest` is already in `Cargo.toml`, along with `base64`, `enigo`, `arboard` (Steps 3–4 deps). `whisper-rs` is **not**. |

---

## 3. The build break

```
error[E0599]: no method named `name` found for struct `Device`
   --> src\audio\engine.rs:316:16
    |
316 |         device.name().unwrap_or_else(|_| "<unknown>".into()),
```

**Cause:** cpal 0.18 removed `DeviceTrait::name()`. It is replaced by
`DeviceTrait::description() -> Result<DeviceDescription, Error>`, where
`DeviceDescription::name() -> &str`.

**Fix** (verified against the vendored source of cpal 0.18.1):

```rust
device
    .description()
    .map(|d| d.name().to_string())
    .unwrap_or_else(|_| "<unknown>".into()),
```

`DeviceTrait` also requires `Display`, so `format!("{device}")` is an acceptable
shorter alternative.

Note that the *adjacent* cpal-0.18 assumption in the same file is correct and
should be left alone — `config.sample_rate` really is a plain `u32` in 0.18
(no `.0` accessor), and the code already accounts for that.

### Also emitted (warnings, non-blocking)

- `src/audio/mod.rs:11` — unused re-export `DeviceFormat`
- `src/audio/mod.rs:12` — unused re-export `TARGET_SAMPLE_RATE`
- `src/pipeline/mod.rs:69` — unused re-export `PIPELINE_EVENT`

All three are re-exports that Step 2 will consume. Leave them or `#[allow]`
them; deleting them means adding them back.

---

## 4. Wired vs. orphaned modules

This is the single biggest trap in the repo. Four Rust files exist with real,
tested-looking code but are **not declared in `lib.rs`**, so they are not
compiled, not type-checked, and their tests do not run.

`src-tauri/src/lib.rs` declares exactly seven modules:

```rust
mod audio;  mod capture;  mod commands;  mod error;
mod hotkey; mod pipeline; mod settings;
```

| File | Status | Notes |
|---|---|---|
| `audio/{mod,engine,buffer,resample}.rs` | ✅ wired | contains the one compile error |
| `capture/{mod,screen}.rs` | ✅ wired | |
| `pipeline/{mod,session,events}.rs` | ✅ wired | the supervisor — start reading here |
| `commands.rs` `error.rs` `hotkey.rs` `settings.rs` | ✅ wired | |
| **`http.rs`** | ⛔ orphaned | shared `reqwest::Client` (`OnceLock`), error describe/truncate helpers |
| **`secrets.rs`** | ⛔ orphaned | API key storage: env var → `secrets.json`, with masking |
| **`stt/mod.rs`, `stt/cloud.rs`** | ⛔ orphaned | Groq/OpenAI Whisper client + prompt biasing |
| **`llm/prompt.rs`** | ⛔ orphaned | cleanup/transform system prompts + snippet expansion |

### The orphaned code will not compile as-is

Wiring these in is not a one-line `mod` declaration. Known blockers:

1. **`llm/` has no `mod.rs`.** Only `llm/prompt.rs` exists, so `mod llm;` fails
   outright. You need `src/llm/mod.rs` with at least `pub mod prompt;`.

2. **Three settings fields are referenced but do not exist** on
   `settings::Providers` (which today has only `stt`, `cleanup`, `web_target`):

   | Referenced at | Field needed |
   |---|---|
   | `stt/cloud.rs:45` | `providers.stt_model: String` |
   | `stt/cloud.rs:63` | `providers.stt_language: String` |
   | `stt/mod.rs:110` | `providers.local_model_path: String` |

   Adding them means updating `Providers`, its `Default`, and the mirrored
   `Settings` interface in `src/lib/ipc.ts` (see §6).

3. **The `local-stt` cargo feature is not declared.** `stt/mod.rs` gates its
   local-Whisper path on `#[cfg(feature = "local-stt")]`, but `Cargo.toml` has
   no `[features]` section and no `whisper-rs` dependency. Until both are added,
   only the `#[cfg(not(feature = ...))]` stub compiles — which is fine, but the
   doc comment promising `cargo build --features local-stt` is currently false.

The `AppError` enum in `error.rs` is already extended for all of this
(`MissingCredential`, `Provider`, `ProviderShape`, `EmptyTranscript`,
`NoSelection`, `Inject`, `Clipboard`, `Http`), so no error work is needed.

---

## 5. Architecture invariants — do not break these

The full reasoning lives in `ARCHITECTURE.md` §1. The short version — these are
load-bearing, and violating one reintroduces a class of bug the design removed:

1. **The hotkey callback does no work.** It looks up an intent in a map and does
   `tx.send(Trigger)`. Nothing else. It runs on the thread pumping the win32
   message loop / macOS run loop; blocking it stalls every global shortcut on
   the machine, and macOS will kill the event tap.

2. **There is no shared mutable session state.** The in-flight session is a
   plain `Option<Active>` local to the supervisor task in `pipeline::spawn`.
   Exactly one task touches it. Do not "improve" this into a
   `Mutex<Option<Session>>` — that reintroduces the hold-a-lock-across-`.await`
   deadlock the design exists to avoid.

3. **Every stage tags its output with `session_id`.** Late results from a
   superseded session are discarded by id check, not prevented by locking.
   `CaptureBuffer::finish(id)` returns `None` for a stale id rather than handing
   over the wrong recording.

4. **The mic device is owned by one OS thread for the process lifetime.**
   `cpal::Stream` is `!Send` on macOS and blocks on drop. It never crosses a
   thread boundary and is never owned by a session. `start_session` only mutates
   the shared buffer — that is why it costs microseconds instead of the
   20–200 ms a WASAPI/CoreAudio device open costs.

5. **The screenshot is spawned at key-*down*, not key-up.** This is where the
   parallelism actually is. `tokio::join!` in `finalize` just collects two
   already-running legs. Moving the spawn to key-up serialises the screenshot
   onto visible latency and defeats the whole design.

6. **Blocking work goes to `spawn_blocking`, never an async worker.** Applies to
   screen capture (DXGI / CoreGraphics), resampling, PNG encoding, and — when it
   lands — local Whisper inference and any `enigo`/`arboard` call.

7. **Finalization is detached.** The supervisor never awaits it, so hotkey
   presses during a slow STT round trip still get processed.

8. **Buffers are capacity-capped.** `max_utterance_secs` bounds the session
   vector; on overflow it stops appending and sets `overflowed` rather than
   growing. `Active::drop` cancels its token and aborts the capture handle.

9. **The realtime audio callback never allocates.** It converts, averages across
   channels, and does one bounded push. `parking_lot::Mutex` held for ~480
   samples. If dropouts ever appear under load, swap in an SPSC ring (`ringbuf`)
   — `CaptureBuffer`'s public API is designed not to change.

10. **A dropped UI event must never fail a recording.** `PipelineEvent::emit`
    returns `()` and logs on failure. The app normally runs with no window open.

---

## 6. The IPC contract

Two files must be edited together. There is no codegen.

- Rust: `src-tauri/src/{settings,commands}.rs` and `pipeline/events.rs`
  (all `#[serde(rename_all = "camelCase")]`)
- TypeScript: `src/lib/ipc.ts` — hand-mirrored types

`src/lib/ipc.ts` is the **only** file allowed to call `invoke`. If a command is
not wrapped there, it does not exist as far as the UI is concerned.

### Commands (11)

`get_settings` · `update_settings` · `runtime_status` · `trigger_session` ·
`stop_session` · `cancel_session` · `test_capture` · `set_api_key` ·
`clear_api_key` · `api_key_status` · `list_input_devices`

The secrets commands are deliberately one-way: a key can be written but never
read back over IPC. `api_key_status` returns only presence, source
(env vs. store), and a masked hint (`gsk_…9c1f`).

### Events

One channel: `pipeline://event`, a `#[serde(tag = "type")]` tagged union →
a discriminated union in TS, folded by the reducer in `src/hooks/usePipeline.ts`.

`armed` → `latched?` → `recording`(100 ms ticks) → `captureDone` →
`finalizing` → `ready` → `idle`, plus `cancelled` and `failed`.

**`ready` is the extension point.** Steps 2–5 hang STT → cleanup → injection off
that exact spot in `pipeline::finalize` (`pipeline/mod.rs:366`).

The reducer drops any event whose `sessionId` differs from the current one —
mirror of invariant #3 on the frontend side.

---

## 7. Files on disk at runtime

Config dir is `com.whisperscreen.app` (from `tauri.conf.json` `identifier`):

- Windows: `%APPDATA%\com.whisperscreen.app\`
- macOS: `~/Library/Application Support/com.whisperscreen.app/`

| File | Owner | Notes |
|---|---|---|
| `settings.json` | `settings.rs` | Atomic write (temp + rename). Every field `#[serde(default)]`, so old/truncated configs still load. Corrupt files are renamed to `settings.json.bad`, never silently overwritten. `sanitize()` clamps on load *and* on every update. |
| `secrets.json` | `secrets.rs` | **Legacy only.** Keys now live in the OS credential store (see below); this file exists solely so migrate-on-read can find keys written by pre-keychain builds. Each migrated key is written through to the keychain and its plaintext entry removed; the file is deleted once empty. |

API keys are **not** files at all anymore: they live in the OS credential
store (Windows Credential Manager / macOS Keychain) via the `keyring` crate,
under service `com.whisperscreen.app` with account names `groq`, `openai`,
`anthropic` (see §11). Env vars (`GROQ_API_KEY`, `OPENAI_API_KEY`,
`ANTHROPIC_API_KEY`) are still checked first and win.

Rust owns settings exclusively. The frontend never writes config directly — it
calls `get_settings`/`update_settings` and re-renders from what Rust returns.
Do not add `plugin-store` alongside this; split-brain config is the exact
failure mode the single-owner rule prevents.

---

## 8. Defaults worth knowing

| Setting | Default | Why |
|---|---|---|
| `hotkeys.dictate` | `Alt+Space` | free on both platforms; Cmd+Space is Spotlight |
| `hotkeys.bridge` | `Alt+Shift+Space` | |
| `hotkeys.transform` | `Alt+Shift+T` | |
| `hotkeys.cancel` | `Alt+Escape` | **not** bare Escape — a global Escape binding would swallow the key from every app on the machine |
| `behavior.tapToggleMs` | 300 | under it → tap latches recording on; over it → push-to-talk |
| `audio.prewarm` | true | costs a permanently lit OS mic indicator; that trade is the user's call |
| `audio.prerollMs` | 400 | recovers the clipped first syllable |
| `audio.maxUtteranceSecs` | 120 | bounds memory; clamped to 5–600 |
| `capture.maxEdgePx` | 1568 | Claude's limit; `0` = native |
| `providers.stt` | Groq | only hosted Whisper fast enough for the 800 ms budget |

Only `Intent::Bridge` takes a screenshot. `Dictate` skips it — the hotkey *is*
the intent, so this is known at t=0 and costs nothing to decide.

---

## 9. Commands

```sh
npm install                  # done; node_modules present
npm run build                # tsc -b && vite build      ✅ passes
npm run lint                 # oxlint                    ✅ passes
npm run tauri dev            # ❌ blocked on §3
npm run rust:test            # ❌ blocked on §3
npm run rust:check           # cargo clippy --all-targets
```

First `cargo build` compiles several hundred crates. The dependency graph is
already downloaded and `Cargo.lock` now exists, so it will be faster than the
3–8 min the README warns about, but still minutes.

Profiles: dev is `opt-level = 1` (at 0, the per-frame downmix risks dropping
samples during `tauri dev`); release is `lto = true`, `opt-level = "s"`,
`panic = "abort"`, `strip = true`.

### Test inventory (45 `#[test]` total)

| | |
|---|---|
| In wired modules | **28** — buffer 4, audio/mod 2, resample 5, screen 5, events 3, session 4, settings 5 |
| In orphaned modules | **17** — prompt 7, secrets 4, http 3, stt/mod 3 |

All 83 tests executed green on 2026-07-28 (`cargo test --lib`). See §1 for the
Smart App Control caveat.

---

## 10. Roadmap

`ARCHITECTURE.md` §3 has the full table. Current position: **all seven steps are
built, compile clean, and pass 83 unit tests. Nothing has run on hardware yet** —
the remaining work is real-device verification (below) and the cross-cutting
items in §13.

| Step | Adds | State |
|---|---|---|
| 1 | hotkeys, parallel capture, pre-roll, tray, settings | ✅ built + tested |
| 2 | `stt/` — Groq/OpenAI Whisper; `whisper.cpp` offline (`local-stt` feature) | ✅ wired into `finalize`; `Transcribed` event + `stt_ms` timing; cancel races the request |
| 3 | `llm/` — cleanup client (Groq/OpenAI/Ollama one-shape, Anthropic separate), snippet expansion, degrade-to-raw fallback | ✅ built + tested |
| 4 | `inject/` — `enigo.text()` ≤100 chars, clipboard-paste longer, `SavedClipboard` restores in `Drop`; macOS errors carry an Accessibility hint | ✅ built + tested |
| 5 | `bridge/` — opener → async delay (`behavior.bridge_delay_ms`) → clipboard image → paste → type query; no auto-submit | ✅ built + tested |
| 6 | `capture/selection.rs` — selection read at key-down via copy chord; `NoSelection` guard; transform via shared `llm::client::complete` | ✅ built + tested |
| 7 | settings UI (5 tabs), secrets commands (no key material over IPC), keychain via `keyring` 3.6 with migrate-on-read from legacy `secrets.json` | ✅ built + tested |

**§13 is the step-by-step work order** — what to build, in what order, with the
exact APIs and acceptance criteria for each task.

---

## 11. Known gaps and decisions already made

Recorded so they are not re-litigated:

- **API keys live in the OS credential store.** Shipped (was: plaintext
  `secrets.json`). `secrets.rs` uses the `keyring` crate (3.6.x) behind the same
  four functions (`get`/`set`/`clear`/`status`); nothing above the module
  changed. Migration is one line of policy — on a keychain miss, `get` checks
  the legacy `secrets.json`, writes any key it finds through to the keychain,
  and removes the plaintext entry (deleting the file once empty). If the
  keychain write fails, the plaintext copy is kept and retried next run rather
  than destroying the user's only copy. Env vars still win over the store.
  Keychain calls block, so all four functions are async wrappers over
  `spawn_blocking` bodies (invariant #6). `status` stays masked and read-only —
  rendering the settings window must not mutate disk.
- **Bare `Escape` to cancel** needs Escape registered *only while a session is
  live*. Until then the default is `Alt+Escape`.
- **`spawn_blocking` cannot be interrupted.** A DXGI call already in flight runs
  to completion; the result is discarded via session-id check. `abort()` releases
  the handle, not the thread. This is acknowledged, not a bug to fix.
- **macOS permissions** (screen recording, mic, Accessibility for Step 4's
  synthetic keystrokes) each need `Info.plist` usage strings and a first-run
  onboarding flow. Accessibility in particular cannot be requested silently.
  None of this exists yet.
- **The CSP in `tauri.conf.json` has no outbound `connect-src`.** This is
  correct and should not be "fixed" — all provider HTTP happens in Rust via
  `reqwest`, never in the webview.
- **The batch shortcut registration is all-or-nothing.** `on_shortcuts` registers
  as a unit, so one conflict fails all four. `hotkey.rs` handles this by
  reporting every accelerator as failed rather than claiming bindings it does
  not hold — partial success is the norm, not an edge case.
- **Resampling is dependency-free** (box-filter decimation for integer ratios,
  pre-average + lerp otherwise). Good enough that STT accuracy is
  indistinguishable. If you ever need VAD or diarisation on the raw signal, drop
  in `rubato`'s `SincFixedIn` behind the same signature.

---

## 12. Environment as verified

```
Windows 11 Home 10.0.26200
rustc / cargo 1.97.1
node v24.18.0, npm 12.0.1
```

MSVC C++ build tools are present and working (cargo linked several hundred
crates successfully). WebView2 ships with Windows 11.

Not a git repo — `git init` before any substantial change so this work is
recoverable. `.gitignore` already covers `node_modules`, `dist`, and
`src-tauri/target`. Note it does **not** cover `secrets.json`, which is harmless
today (that file lives in the app config dir, not the repo) but would matter if
anyone ever moves it.

---

# 13. The work order

Everything below is what this product still needs, in dependency order. Each task
lists the files to touch, the exact APIs (verified against the vendored crate
sources on 2026-07-28, not recalled), and how to know it's done.

**Read §5 before writing any Rust.** Those ten invariants are the design; a task
here that appears to conflict with one of them is a misread of the task.

## Rules for whoever picks this up

1. **Do not start Task 2 until Task 0 is green.** Every line of Step 1 is
   unverified — it has never compiled or run. Building on it before it's proven
   means debugging two unknowns at once.
2. **One task per commit**, and `git init` first (§12) — none of this is
   recoverable right now.
3. **Never invent a crate API.** These are pinned, current versions with genuine
   breaking changes from what a model is likely to recall — `cpal 0.18` already
   broke this build once (§3). If a signature isn't listed below, read it in
   `~/.cargo/registry/src/index.crates.io-*/<crate>-<version>/`.
4. **Every new module gets unit tests in the same file** (`#[cfg(test)] mod tests`)
   — that's the existing convention in all 11 tested files, and it is why the
   orphaned modules already have 17 tests.
5. **Every IPC change touches two files.** Rust struct + `src/lib/ipc.ts`. There
   is no codegen (§6).
6. **Keep the module doc comments.** Every file here opens with a `//!` block
   explaining *why*, not what. It's the most valuable thing in the repo — match
   it.

---

## Task 0 — Make it compile `[BLOCKS EVERYTHING]`

**File:** `src-tauri/src/audio/engine.rs:316`

Apply the fix in §3 (`device.description()` → `.name()`). Then:

```sh
npm run rust:test     # 28 tests must pass
npm run rust:check    # clippy, --all-targets
```

**Done when:** 28 tests pass and the app launches to a tray icon.

**Then verify Step 1 on real hardware** — every claim in `README.md` is currently
unobserved. Walk its five checks (capture budget, simultaneity, pre-roll, the
120 s cap, supersede). Record what you actually measure; if a number is wrong,
fix the README rather than leaving an aspirational one in place.

---

## Task 1 — Wire the orphaned modules

**Goal:** get `http`, `secrets`, `stt`, and `llm` into the build. No behaviour
change — the pipeline still ends at `Ready`.

1. **Create `src-tauri/src/llm/mod.rs`:**
   ```rust
   //! Cleanup and transformation via a chat model.
   pub mod prompt;
   ```
2. **Extend `settings::Providers`** with the three missing fields (§4.2). Suggested
   defaults, matching the module docs:
   ```rust
   pub stt_model: String,        // "whisper-large-v3-turbo"
   pub stt_language: String,     // "en"
   pub local_model_path: String, // ""
   ```
   Update `Default for Providers`, and mirror all three into the `Settings`
   interface in `src/lib/ipc.ts`.
3. **Declare the modules** in `lib.rs`: `mod http; mod llm; mod secrets; mod stt;`
4. **Add the `local-stt` feature** to `Cargo.toml` so the `#[cfg]` in `stt/mod.rs`
   is honest:
   ```toml
   [features]
   local-stt = ["dep:whisper-rs"]

   [dependencies]
   whisper-rs = { version = "0.14", optional = true }
   ```
   Verify that version resolves before committing; if `whisper-rs` proves painful
   (it builds a C++ library), it is legitimate to defer this and delete the
   `LocalWhisper` variant instead — just don't leave a `#[cfg]` on an undeclared
   feature.
5. Fix the compile fallout. Expect unused-import warnings to *disappear* here —
   the three in §3 are consumed by this task.

**Done when:** `cargo test` runs **45** tests, all passing.

---

## Task 2 — STT into the pipeline

**Files:** `pipeline/mod.rs`, `pipeline/events.rs`, `src/lib/ipc.ts`, `src/App.tsx`

Attach transcription at `pipeline/mod.rs:366`, where the comment already marks
the seam. Inside `finalize`, after `captured` is in hand:

```rust
let wav = audio::encode_wav(&captured.samples, captured.sample_rate)?;
let transcript = stt::transcribe(&app, &settings, wav, &dictionary).await?;
```

`audio::encode_wav` already exists and is tested — it has simply never been
called. That is the intended hand-off.

Three things to get right:

- **`finalize` must keep the session-id discipline.** It already returns early on
  `AppError::Superseded`; the STT call goes *after* that check, never before.
- **Emit a new `PipelineEvent::Transcribed` before `Ready`** so the UI can show
  text while cleanup runs. Add the variant, mirror it in `ipc.ts`, and handle it
  in the `usePipeline` reducer with the same stale-`sessionId` guard every other
  variant uses.
- **Extend `Timings`** with `stt_ms` (`Transcript` already carries `elapsed_ms`).
  The 800 ms budget is only observable if each stage reports its own cost.

**Cancellation:** `Active::cancel` is a `CancellationToken` that nothing currently
awaits. This is where it earns its place — select on it against the STT future so
`Alt+Escape` aborts an in-flight request instead of letting it land.

**Done when:** hold the dictate hotkey, speak, and the transcript appears in the
panel with a per-stage latency breakdown.

---

## Task 3 — Cleanup (`llm/`)

**New file:** `src-tauri/src/llm/client.rs`

`llm/prompt.rs` already has the prompts and the snippet expander, all tested.
What's missing is the HTTP client. Order of operations per the module doc:
**expand snippets in Rust first** (exact, free), *then* send to the model.

`CleanupProvider::None` must short-circuit and return the transcript untouched —
that's the documented ~200 ms saving, and it's also your fallback when a provider
errors. **A failed cleanup must never lose the transcript**; degrade to the raw
text and log, exactly as `flatten_capture` degrades a failed screenshot.

### Provider contracts

Groq, OpenAI, and Ollama all speak the OpenAI `/chat/completions` shape, so one
client covers three — same trick `stt/cloud.rs` already uses for two:

| Provider | Endpoint | Auth |
|---|---|---|
| Groq | `https://api.groq.com/openai/v1/chat/completions` | `Authorization: Bearer` |
| OpenAI | `https://api.openai.com/v1/chat/completions` | `Authorization: Bearer` |
| Ollama | `http://localhost:11434/v1/chat/completions` | none |

System prompt goes in `messages[0]` with `"role": "system"`. Response text is at
`choices[0].message.content`.

**Anthropic is a different shape** — do not try to force it through the same
struct:

```
POST https://api.anthropic.com/v1/messages
x-api-key: <key>
anthropic-version: 2023-06-01
content-type: application/json

{"model": "...", "max_tokens": 1024,
 "system": "<POLISH_SYSTEM>",
 "messages": [{"role": "user", "content": "<transcript>"}]}
```

Four differences that will bite a naive port from the OpenAI path:

1. **`system` is a top-level field**, not a message role.
2. **`max_tokens` is required.**
3. **Response is `content[]`, a block array** — read `content[0].text` after
   checking `content[0].type == "text"`.
4. **Do not send `temperature`.** `stt/cloud.rs` sends `temperature: 0` to the
   Whisper endpoint, which is correct there. Copying that to the Anthropic chat
   endpoint **returns a 400** on `claude-opus-5` and `claude-sonnet-5` —
   sampling parameters were removed on those models. `claude-haiku-4-5` still
   accepts it.

**Model IDs** (current as of 2026-07-28): `claude-opus-5`, `claude-sonnet-5`,
`claude-haiku-4-5`. Use the exact string — these are complete IDs, never append a
date suffix.

> ⚠️ **Latency trap specific to this app.** On `claude-opus-5`, extended thinking
> is **on by default** when the `thinking` field is omitted. For a dictation
> cleanup pass inside an 800 ms budget that is fatal. Either send
> `"thinking": {"type": "disabled"}` (accepted at effort `high` or below), or use
> `claude-haiku-4-5`, which is the better latency fit for this stage anyway.
> Whichever you choose, `settings.providers` needs a `cleanup_model` field —
> hardcoding a model ID here is the same mistake `stt_model` already avoids.

**Done when:** "um, send this to Alex, actually Sarah, new line thanks" becomes
"Send this to Sarah.\nThanks." — and pulling the network cable yields the raw
transcript rather than an error.

---

## Task 4 — Injection (`inject/`)

**New files:** `src-tauri/src/inject/{mod,keyboard,clipboard}.rs`

`enigo` and `arboard` are already in `Cargo.toml`. Both talk to the OS on the
calling thread and block — **every call goes through `spawn_blocking`**
(invariant #6).

Verified APIs (enigo 0.6.1, arboard 3.6.1):

```rust
use enigo::{Enigo, Keyboard, Settings};       // Keyboard trait must be in scope
let mut enigo = Enigo::new(&Settings::default())?;   // -> Result<_, NewConError>
enigo.text("hello")?;                                 // -> InputResult<()>

use arboard::Clipboard;
let mut cb = Clipboard::new()?;
let saved = cb.get_text().ok();      // may legitimately be empty — don't error
cb.set_text("...")?;
```

**Save and restore the clipboard around any paste.** Silently destroying what the
user had copied is the single worst failure mode in this class of app. `get_text`
returning `Err` on an empty or image-only clipboard is normal — treat it as
`None`, not a failure.

**Prefer `enigo.text()` over synthesised key events** for typing: it handles
Unicode and non-US layouts, which per-key synthesis does not.

macOS needs Accessibility permission and **cannot request it silently** (§11).
Ship a first-run check that detects the missing grant and links to the pane,
rather than typing into the void.

**Done when:** `Alt+Space` types cleaned text into Notepad, VS Code, and a browser
address bar, and the clipboard afterwards holds exactly what it held before.

---

## Task 5 — The web bridge (`bridge/`)

**New file:** `src-tauri/src/bridge/mod.rs`

The payoff feature, and the only one that uses `Intent::Bridge`'s screenshot.

`CaptureFrame` → `arboard::ImageData` is a direct hand-off — both are RGBA8, 4
bytes per pixel, so the only work is a `usize` cast:

```rust
use arboard::ImageData;
use std::borrow::Cow;
cb.set_image(ImageData {
    width: frame.width as usize,
    height: frame.height as usize,
    bytes: Cow::Borrowed(&frame.rgba),
})?;
```

This is exactly why `capture/screen.rs` defers PNG encoding (§ its module doc) —
the clipboard path never encodes. Only reach for `frame.to_png()` if you later
add an HTTP upload path.

Flow: open/focus `settings.providers.web_target.url()` via `tauri-plugin-opener`
(already a dependency) → wait for focus → set clipboard image → paste → type the
prompt → submit.

The fragile part is timing, not the clipboard: there is no reliable signal that a
browser tab has finished loading and focused its input. Make the delay a setting
rather than a magic constant, and fail visibly if the paste target never focuses.

**Done when:** `Alt+Shift+Space` + "what's wrong with this error" lands the
screenshot *and* the question in a Claude conversation.

---

## Task 6 — Selection transform (`capture/selection.rs`)

`Intent::Transform` and `Intent::needs_selection()` already exist and are tested;
nothing calls them. `capture/mod.rs` is a directory specifically to host this.

Read the highlighted text by saving the clipboard, sending Ctrl/Cmd+C, reading,
then restoring — same discipline as Task 4. `AppError::NoSelection` is already
defined for the empty case.

Then `llm::prompt::transform_user(instruction, selection)` frames it — note the
`<<<TEXT` delimiters are deliberate prompt-injection defence, so a selection
containing "ignore the above" doesn't read as an instruction. Don't "simplify"
that framing away.

**Done when:** selecting a scruffy paragraph and saying "make this formal"
replaces it in place.

---

## Task 7 — Settings UI and keychain

- **`src/views/`** — `SettingsPanel`, `ProvidersTab`, `DictionaryTab`. Already
  named in `ARCHITECTURE.md`'s file map; the directory doesn't exist yet.
  `App.tsx` is a Step-1 diagnostic panel, not the real UI.
- **API key entry** — `secrets.rs` already exposes exactly the four functions the
  UI needs (`get`/`set`/`clear`/`status`), and `status` returns masked hints that
  are safe to render. Expose them as Tauri commands; **never** return key material
  to the frontend.
- **Keychain migration** — swap `secrets.json` for the `keyring` crate behind
  those same four functions (§11). Nothing above that module changes.

---

## Cross-cutting, once the pipeline is whole

- **Bare `Escape` to cancel** — register it *only* while a session is live
  (§11). Needs `hotkey.rs` to grow dynamic register/unregister; the plugin's
  all-or-nothing batch behaviour (§11) makes this less trivial than it sounds.
- **Sound cues** — `settings.behavior.sound_cues` is read by nothing.
- **macOS `Info.plist`** usage strings for mic, screen recording, and
  Accessibility, plus a first-run onboarding flow (§11). Nothing macOS-specific
  has been tested; the app has only ever been built on Windows.
- **`AppError` variants land as toasts** — `Provider { status, body }` carries
  the real reason (bad key, rate limit, model not found). Surface it; don't
  collapse it to "something went wrong".

## Definition of done for v1

A user can: install it, paste one API key, and hold `Alt+Space` to dictate clean
text into any app in under 800 ms — then hold `Alt+Shift+Space` to ask a web LLM
about whatever is on their screen. Everything else is secondary to those two
paths working reliably.
