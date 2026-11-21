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
