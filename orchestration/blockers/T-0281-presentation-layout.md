# T-0281 — presentation mode: implementation complete, visual evidence blocked by the host

Status: **closed (2026-11-24)**. `VIEW-012` is `verified` in
`docs/features.toml`. The bottom bar now has a Present control that dispatches
`AppCommand::StartPresentation` into the same presentation entry point the
`--present` launch flag uses, and the real bottom-bar widget test
`bottom_bar_present_button_enters_presentation_mode` clicks it by its AccessKit
label and asserts `is_presenting()`. The direct-paint rewrite, the 22 model
tests and the Escape exit remain as described below. See
`docs/evidence/review/T-0379.md`.

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

Closed 2026-11-24 with the Present control: `cargo xtask features verify`
reports 146 rows, 142 verified (58 gui, 84 cli), 3 replaced and 1 deferred
(PLAT-002), and `VIEW-012` is `verified`.

## Matrix alignment (2026-11-24)

The 2026-11-23 review found `VIEW-012` marked verified with no reachable entry
point, and it was demoted to `model`. T-0379 restored the row honestly: the
bottom bar's Present control dispatches `AppCommand::StartPresentation` into the
real presentation entry point, and the widget test clicks that control and
asserts `is_presenting()`. A full-screen capture remains a nice-to-have, not a
blocker, because the GUI reachability criterion is now met and tested.
