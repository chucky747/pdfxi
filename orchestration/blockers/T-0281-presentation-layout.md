# T-0281 — presentation mode: page fit is still wrong (VIEW-012 not claimed)

Status: open. `VIEW-012` stays `model`. The launch shell works; the page view does
not, so nothing is claimed.

## What works (verified by capture)

`pdfxi-ui.exe --present <file>` now:

- switches the window to full screen (2560x1440 on this host) via
  `ViewportCommand::Fullscreen(true)` on the first frame;
- hides the title bar, command bar, rail, pages panel, tools panel and status
  bar, leaving a black canvas;
- maps `Right/Down/Space/PageDown` to next page, `Left/Up/PageUp` to previous,
  and `Escape` to leave presentation mode and restore decorations;
- is parser-tested: `settings::tests::presentation_flags_are_bare_switches_and_ignore_lookalikes`.

## What is wrong

The page is drawn at a scale larger than the window, so it is clipped on the
right. Two captures:

- `target\tmp\present-5.png` — computed custom zoom: one page, clipped.
- `target\tmp\present-50.png` — forced 50%: the *continuous* column of all three
  pages is visible, offset to the right side of the canvas.

So the custom zoom is applied, but the canvas layout is not a centred single
page: it keeps the continuous column and the horizontal padding is computed
against a wider content box than the visible viewport.

## Attempts recorded

1. `fit_page()` on entry — no effect (fit scale is caller-supplied and the
   first frames still used the pre-fullscreen viewport).
2. `ViewportCommand::Fullscreen(true)` then fit on the next frame — fullscreen
   now applies, page still clipped.
3. Custom zoom from `ctx.content_rect()` and `page_sizes[0]` — zoom applies
   (50% test changed the scale) but the column layout and offset remain wrong.
4. Sent an extra repaint after the fullscreen command — no change.

## Next approach (not yet done)

Drive the active tab to the single-page layout (`view.rs` has
`SinglePage`/`Continuous`/`TwoPageContinuous`) before fitting, and centre the
active page in the visible canvas instead of the continuous column; or paint the
current slide directly from the raster cache with an explicit destination rect
instead of reusing `canvas_ui`.

## Effect on the goal

No feature row changes. `cargo xtask goal-verify --all` still reports
`FEATURES OK: 145 rows, 63 verified`. The campaign continues on other rows while
this one is reworked.

## Attempt 5-6 (single-page layout and per-frame fit)

- Forced `ViewMode::SinglePage` on entry (restored on exit) and applied a custom
  zoom from `ctx.content_rect()` and `page_sizes[0]` on every presentation frame.
- Result: `target\tmp\present-7.png` (real screen capture, not PrintWindow) is
  pixel-identical to the earlier captures: one page, scale ≈ 2560/792 ≈ 3.23,
  clipped at the right edge.
- `fmt`/`clippy -D warnings` pass. The zoom call compiles and runs (the guard is
  satisfied because a page is painted, so `layout_for_state` has page sizes).
- The scale matches fit-**width**, not fit-page, so the suspicion is now inside
  `CanvasLayout::with_rotation`/`Zoom` resolution: either the viewport passed to
  the layout is wider than the window, or `Zoom::FitPage` resolves the width
  constraint first.

## Next diagnostic

Add a temporary debug overlay (or a unit test) that prints
`canvas.viewport`, `zoom`, the resolved scale and `layout.pages()[0].rect` for
`ViewMode::SinglePage` at the presentation viewport, then fix the resolution at
the `CanvasLayout` level rather than in `ui.rs`. Do not call VIEW-012 verified
until a full-screen capture shows the whole page with black margins and arrow
navigation changes the slide.

## Attempts 7-9 (instrumented; fit fixed, centring and key proof remain)

Debug overlay printed `vp=2560x1440 ppp=1.50 page=720x405 zoom=...`. The fixture
pages are 720x405, so a pixel-ratio-corrected fit is
`min((2560/1.5)*0.94/720, (1440/1.5)*0.94/405) = 2.23`.

- Applying the ratio-corrected custom zoom **fixes the clipping**: the whole
  slide is visible (`target\tmp\present-final2.png`).
- The slide is **right-aligned** (page starts at x≈730, flush to the right
  edge). Setting `scroll_x` positive is clamped and does not centre it, and
  `pad_x` in `canvas_ui` still centres against a wider content box.
- Arrow-key navigation could not be proven end to end: `SetForegroundWindow`
  from this process does not take focus (same host restriction as the Help
  window), so injected Right/Left keystrokes never reach the egui input. The
  capture after two Right presses is still slide 1.

## Remaining work for VIEW-012

1. Centre the slide horizontally (fix `pad_x`/content width for `SinglePage`,
   or set the scroll offset in the same units the canvas expects).
2. Prove key navigation without OS focus: extract the key decision into a pure
   helper and unit-test it (Right/Space/PageDown -> next, Left/PageUp ->
   previous, Escape -> exit), and/or post `WM_KEYDOWN` to the window.
3. Only then add captures for slide 1 -> 2 -> 1 and the VIEW-012 fragment.

The row stays `model`; nothing is claimed.
