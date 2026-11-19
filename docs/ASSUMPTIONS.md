# ASSUMPTIONS

Decisions made without blocking the user. Replace with an ADR when they become
cross-cutting.

1. **Windows-first.** The development host is Windows 11 x64. macOS/Linux support
   follows after M5.
2. **GNU bootstrap on this host.** MSVC Build Tools require an elevated install,
   which is unavailable in the autonomous shell. The workspace pins
   `stable-x86_64-pc-windows-gnu` and a user-local WinLibs MinGW toolchain.
   CI adds an MSVC job so the code stays portable.
3. **Git via MinGit.** Winget's Git installer requests elevation; a user-local
   MinGit extraction is used and added to the user PATH.
4. **PDFium as the default renderer.** It is the closest open implementation to
   Acrobat's rendering behavior. The `Renderer` trait keeps a native compositor
   possible.
5. **lopdf for object manipulation.** It exposes the object graph needed for
   editing and incremental save.
6. **egui/eframe for the UI.** Fast iteration, custom canvas, and accesskit
   support. A future native shell can replace it behind the app crate.
7. **Local replacements for retired Adobe cloud services.** FormsCentral,
   EchoSign, Acrobat.com, SendNow, and LiveCycle Rights Management are not
   recreated as remote services.
8. **Tesseract for OCR.** Bundled as an optional sidecar; a pure-Rust fallback is
   used in tests.
9. **No Adobe binary plug-in compatibility.** PDFXI exposes a WASM/JSON plugin
   API instead of loading Adobe plug-ins.
10. **Context cap.** DeepSeek V4.1 Flash is configured with a 588,000-token hard
    context and a 15% compaction buffer, yielding a ~499,800-token compaction
    ceiling.
11. **Doctest execution is host-limited.** The current Windows host denies
    execution of generated doctest binaries with `os error 5`. Rustdoc examples
    remain in the documentation but use non-executed `text` blocks in crates
    that would otherwise fail `cargo test`. Unit and integration tests carry the
    behavioral coverage. Revisit when the host execution policy is understood.
