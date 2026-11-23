# ADR 0010: Closing the window after the unsaved-changes prompt

- Status: accepted
- Date: 2026-11-23
- Task: T-0369

## Context

The 2026-11-23 review found that closing the window silently discarded work.
The first fix cancels the platform close while a dirty tab is prompted, but it
left two defects:

1. `close_prompt_ui` took `pending_close` and dispatched `ResolveClose`
   without restoring it, so every button and keyboard choice was a no-op.
2. eframe 0.33 turns `ViewportCommand::Close` into an app event for the root
   viewport; it does not close the native window. After Discard the tab closed
   but the process stayed open on the Home screen.

## Decision

- Restore `pending_close` before dispatching `ResolveClose`.
- Route the title-bar X through the same dirty-tab prompt as `WM_CLOSE`:
  `begin_window_close_prompt()` opens the prompt for the first dirty tab, and a
  clean window requests the final close.
- On Windows, finish the close by terminating the process only after every dirty
  tab has been saved or explicitly discarded. No unsaved state remains at that
  point, and the app has no eframe `save`/`on_exit` work to run. Other platforms
  keep `ViewportCommand::Close` as the fallback.
- Keyboard choices in the prompt are `D` (discard), `S` (save) and `Escape`
  (cancel), in addition to the buttons.

## Consequences

- Discard/Save and the title-bar X now actually close the viewer.
- A `WM_CLOSE` from the window manager still takes the normal prompt path.
- The process-exit fallback is Windows-specific and can be replaced when
  eframe exposes a working root-viewport close. T-0369 records the eframe
  limitation and the tests that guard the behaviour.
- `ui::widget_tests::close_prompt_discard_closes_the_tab_and_requests_the_window_close`
  fails if the pending prompt is dropped again.
