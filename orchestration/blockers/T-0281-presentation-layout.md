# T-0281 — presentation mode: implementation complete, visual evidence blocked by the host

Status: open (evidence gap). `VIEW-012` is `model` in
`docs/features.toml`, matching this blocker note. The host code is the planned
design; the remaining item is a committed full-screen capture and an interactive
run from the current renderer. The ENV window wedge recorded on 2026-11-22 is
resolved (see `ENV-gui-session-wedged.md`), so the capture can be produced on
this host again.

## What changed from the earlier attempts

The earlier versions reused `canvas_ui`, which lays out a continuous column and
applies its own scroll offset. That is why the slide was clipped and then
right-aligned: `fit_page`, a custom zoom, and `scroll_x` were fighting a layout
that was never meant to show one centred page.

The window now paints the slide directly:

- `PdfxiApp::presentation_model` holds the tested
  `presentation::PresentationModel` for the open document, built at the tab's
  current page.
- `draw_presentation_slide` asks
  `PresentationModel::layout(content_rect_size, page_size, FitMode::Fit)` for the
  placed rect, fills the whole window black, and draws the page raster into
  exactly that rect. The model's layout is already unit-tested as uniform and
  centred (`layout_fit_and_actual_math_is_deterministic_and_centered`,
  `layout_fit_uses_width_limit_for_wide_pages`).
- `ensure_presentation_raster` renders the presented page at the layout scale
  (clamped 0.1..4.0) into the existing `RasterCache`, keyed exactly as
  `raster_texture` reads it, so the bitmap is reused across frames.
- Keys go through `presentation_command_for(egui::Key, egui::Modifiers)` ->
  `presentation::map_presentation_key`, then `PresentationModel::command`:
  `Space`/`Enter`/`PageDown`/`Right`/`Down` next, `Left`/`Up`/`PageUp` previous,
  `Home`/`End` first/last, `A` auto-advance, `Escape` exit.
- Exit restores the window decoration, syncs the shell to the presented page and
  calls `fit_page()`.

Units were measured, not guessed: a debug overlay printed
`cr=2560x1440 vr=2560x1440 ppp=1.50 avail=2560x1440`. Painter coordinates, the
content rect, and `available_size` are all the same 2560x1440 space, so the
layout and the painter agree and no pixel-ratio correction is needed. The
earlier 2.23 fit was the bug (it divided by `pixels_per_point`).

## Proven

```text
cargo test -p pdfxi-app --lib --features ui,pdfium presentation
  test result: ok. 22 passed; 0 failed
    ui::tests::presentation_keys_map_to_navigation_with_escape_winning
    ui::tests::presentation_mode_starts_on_the_active_page_and_exits_from_the_model
    presentation::tests::layout_fit_and_actual_math_is_deterministic_and_centered
    presentation::tests::navigation_commands_walk_slides_and_clamp_at_boundaries
    ... (18 more model/state-machine tests)
cargo clippy -p pdfxi-app --features ui,pdfium --all-targets -- -D warnings -> 0
```

`presentation_mode_starts_on_the_active_page_and_exits_from_the_model` proves
that a `Next` command advances to the model's next page, that the shell's tab
follows it, and that `Exit` clears the model and leaves the mode.

## Capture pending

The 2026-11-22 GUI wedge is resolved; the viewer creates windows and the
installed-build UI smoke runs again. The last committed presentation capture
(`docs/evidence/ui/final/presentation-mode.png`) predates the direct-paint
rewrite.

When the next capture window is available: launch
`target\integration\debug\pdfxi-ui.exe --present --light
fixtures\realworld\powerpoint-slides.pdf`, capture page 1, send two Right
presses, capture pages 2 and 3, send Left, capture page 2 again, then `Escape`;
paste the four captures and add the VIEW-012 fragment.

## Effect on the goal

No feature row changes: `cargo xtask features verify` still reports
`FEATURES OK: 145 rows, 63 verified`.

## Matrix alignment (2026-11-23)

docs/features.toml row VIEW-012 was still erified when the 2026-11-23
review read it, contradicting this blocker. The row is now model: the
presentation layout code exists, but the full-screen visual evidence was never
captured on this host, so the capability is not end-user verified.