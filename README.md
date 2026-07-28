# Whisper Screen

A tray-resident voice utility: one hotkey opens the microphone and captures the
screen **at the same time**, then routes the result either into your active text
cursor or into a web LLM alongside the screenshot.

- **Architecture and the full file map:** [ARCHITECTURE.md](./ARCHITECTURE.md)
- **Start reading the code at:** `src-tauri/src/pipeline/mod.rs`

Stack: Tauri 2.11 · Rust · React 19 + TypeScript 6 · Vite 8

## Status: all modules built

Working now (compile- and test-verified; see "Verify" below):

- Global hotkeys with key-down/key-up, giving push-to-talk *and* tap-to-toggle
  from one binding.
- Simultaneous capture: audio recording and screenshot both start at key-down.
- Pre-warmed microphone with a 400 ms pre-roll ring, so the first syllable is not
  clipped.
- Mono 16 kHz PCM out, WAV encoding in memory, no temp files.
- Silent screenshots, downscaled to 1568 px, ~30 ms.
- **Flow mode** — STT (Groq/OpenAI Whisper, optional local `whisper.cpp`), LLM
  cleanup (filler words, grammar, self-corrections; Groq/OpenAI/Anthropic/Ollama),
  and text injected into the active cursor.
- **Bridge mode** — screenshot + spoken question pasted into ChatGPT, Claude, or
  Gemini via the OS clipboard and synthetic paste.
- **Transform mode** — highlighted text read at key-down, rewritten by the spoken
  instruction, replaced in place.
- Tray icon, hidden settings window with tabs for providers, API keys (stored in
  the OS keychain), hotkeys, audio devices, dictionary and snippets, plus a
  per-stage latency readout.

Runtime behaviour on real hardware (latency numbers, tray flow, web-LLM paste
timing) has not been observed yet — see the hardware checks below.

## Prerequisites

The frontend builds today. For the Rust backend you need both of these:

### Windows

```powershell
winget install Rustlang.Rustup
winget install --id Microsoft.VisualStudio.2022.BuildTools --override "--quiet --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
```

The C++ build tools are not optional — Rust's `msvc` target links through them.
WebView2 is already present on Windows 11.

### macOS

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
xcode-select --install
```

Then restart your shell and confirm:

```sh
rustc --version && cargo --version
```

## Run

```sh
npm install
npm run tauri dev
```

First `cargo build` compiles several hundred crates and takes 3–8 minutes. After
that, incremental builds are seconds.

On first launch the app lives in the tray — the window is hidden by design. Open
it from the tray menu, or press `Alt+Space` and watch the panel.

### Default hotkeys

| Action | Binding |
|---|---|
| Dictate into the active cursor | `Alt+Space` |
| Screenshot + voice → web LLM | `Alt+Shift+Space` |
| Transform selection in place | `Alt+Shift+T` |
| Cancel the in-flight session | `Alt+Escape` |

Hold for push-to-talk; tap to latch recording on, then tap again to stop.

## Verify

```sh
npm run rust:test                 # 83 unit tests across the backend
npm run build                     # tsc -b && vite build
```

In the running app, all of these checks live in the **Status** tab:

1. **Capture budget** — click *Take one screenshot*. Expect under 50 ms; the
   number turns red if not.
2. **Simultaneity** — press `Alt+Shift+Space`, speak for ~3 s, release. The
   *Screenshot leg* card fills in while you are still talking, and *Capture* in
   the *Last session* card shows the screenshot cost, not the total.
3. **Pre-roll** — start speaking, then press the hotkey mid-word. The recorded
   duration exceeds your hold time, because the ring supplied the lead-in.
4. **No leaks** — hold the hotkey for 120 s (the cap). It stops itself, flags
   `truncated`, and RSS plateaus rather than climbing.
5. **Supersede** — tap the hotkey twice quickly. The first session emits
   `cancelled`, not a stale `ready`.

## Settings

One JSON file, owned by Rust; the UI never writes it directly.

- Windows: `%APPDATA%\com.whisperscreen.app\settings.json`
- macOS: `~/Library/Application Support/com.whisperscreen.app/settings.json`

A corrupt file is renamed to `settings.json.bad` and defaults are used, rather
than being silently overwritten.

## Permissions

| | Windows | macOS |
|---|---|---|
| Microphone | Settings → Privacy → Microphone | Prompted on first use |
| Screen capture | none needed | System Settings → Screen Recording (manual) |
| Global hotkeys | none needed | Input Monitoring |
| Synthetic keystrokes (Step 4) | none needed | Accessibility — must be granted manually |
