# PDFXI feature acceptance matrix

This is the acceptance checklist for the campaign in
`docs/campaigns/2026-11-20-feature-acceptance.md` (section 5). Only the
orchestrator ticks an item: it changes `Status` and records the evidence path
after its own check on the installed build, never on a lane's report alone.
Lanes add evidence files, but this table and the ticks belong to the
orchestrator.

`Status` is `open` while an item is unproven, `regression` for the row that was
already shipped and only has to keep working, and `pass` once the orchestrator
has verified it on the installed build with the evidence path in `Evidence`.
`Task` names the task expected to deliver the item; `P1-P3` means the item
lands across several phases.

| ID | Image element / owner item | Passes when | Task | Status | Evidence |
|---|---|---|---|---|---|
| A-01 | Title bar: tabs, new tab, window controls | Already shipped; still works in both control styles | regression | regression | |
| A-02 | Bell (notifications) | Lists real local events; empty state when none | T-0246 | open | |
| A-03 | Help | Opens the bundled user guide | T-0221 | open | |
| A-04 | User avatar | Shows initials from a local display name used as comment author and signer name | T-0221 | open | docs/evidence/ui/T-0221.md |
| A-05 | Undo / redo | Reverts and reapplies every document mutation; disabled when nothing to undo | T-0222 | open | docs/evidence/ui/T-0222.md |
| A-06 | Command bar: Edit, Comment, Highlight, Sign, Organize, Export, AI Assistant | Each activates its working tool | T-0242, T-0243, T-0244, T-0245, T-0231, T-0266, T-0253 | open | |
| A-07 | Rail: Home | Home with recent files and open action | T-0221 | open | docs/evidence/ui/T-0221.md |
| A-08 | Rail: Files | Browses the local library folder; opens PDFs | T-0221 | open | docs/evidence/ui/T-0221.md |
| A-09 | Rail: Recent | Real recent list, persisted | T-0221 | open | docs/evidence/ui/T-0221.md |
| A-10 | Rail: Starred | Star/unstar persists across restarts | T-0221 | open | docs/evidence/ui/T-0221.md |
| A-11 | Rail: Shared | Shows the chosen shared folder; shared review comments sync as files | T-0221, T-0246 | open | |
| A-12 | Storage meter | Real usage of the library folder | T-0221 | open | docs/evidence/ui/T-0221.md |
| A-13 | Rail: Pages + panel header "Pages (n)", grid toggle, add page | Real count; grid view; insert page works | T-0231 | open | |
| A-14 | Thumbnails with selection | Already shipped; plus multi-select and drag reorder | T-0231 | open | |
| A-15 | Rail: Comments with count badge | Panel lists real comments; badge is the real count | T-0242 | open | docs/evidence/ui/T-0242.md (A-32 is T-0246) |
| A-16 | Rail + card + pill: AI Assistant | Ask, summarize, key insights on the open document through OpenCode | T-0252, T-0253 | open | |
| A-17 | Text edit box with handles on a paragraph | Click a paragraph, edit, reflow; saved file shows and extracts the new text | T-0233, T-0264 | open | |
| A-18 | Highlight | Highlight from a text selection; visible in other readers | T-0242 | open | docs/evidence/ui/T-0242.md (A-32 is T-0246) |
| A-19 | Comment note with thread popup, author, time, menu | Add, reply, edit, delete, resolve; persists in the PDF | T-0242 | open | docs/evidence/ui/T-0242.md (A-32 is T-0246) |
| A-20 | Signature with "E-signed" badge | Badge only when the cryptographic check passes on an unmodified document | T-0241, T-0249, T-0245 | open | |
| A-21 | Bottom bar: prev/next, page number box, zoom dropdown, minus / plus, full screen | All work; page box accepts typed numbers | T-0220 | open | docs/evidence/ui/T-0220.md |
| A-22 | Tools panel with search | Search filters the tool list | T-0220 | open | docs/evidence/ui/T-0220.md |
| A-23 | Tool: Edit PDF | Text and image editing (A-17 plus add/move/resize/replace/delete) | T-0265, T-0264 | open | |
| A-24 | Tool: Comment | Markup set of A-18/A-19 | T-0242 | open | docs/evidence/ui/T-0242.md (A-32 is T-0246) |
| A-25 | Tool: Fill & Sign | Fill form fields; place drawn/typed/image signature | T-0243, T-0245 | open | |
| A-26 | Tool: Organize Pages | Reorder, rotate, delete, insert, extract, split | T-0231 | open | |
| A-27 | Tool: Export PDF + Export card (format dropdown, Export button) | Exports to Word, Excel, PowerPoint, image, text, HTML, RTF; Office opens the result | T-0239, T-0244 | open | |
| A-28 | Tool: Convert to PDF | Creates PDFs from images, text, Markdown, HTML and Office files | T-0240, T-0266 | open | |
| A-29 | Tool: Combine Files | Merges several files into one PDF | T-0231 | open | |
| A-30 | Tool: Protect PDF | Set/change/remove password and permissions; redact; sanitize | T-0238, T-0237, T-0257, T-0263 | open | |
| A-31 | Caption: E-Sign "send, sign, and track" | Request signature by email; tracker updates on import of the signed copy | T-0245 | open | |
| A-32 | Caption: Comment & Review "collaborate" | Shared-folder review and XFDF import/export | T-0246 | open | |
| A-33 | Light and dark, both control styles | Every new panel and dialog is legible in all four combinations | every phase gate | open | |
| O-01 | Saving | Save, Save As, dirty marker, close prompt, atomic write | T-0222 | open | docs/evidence/ui/T-0222.md |
| O-02 | Printing | Prints to a real printer through the Windows dialog | T-0223 | open | docs/evidence/ui/T-0223.md (physical Brother print check pending at T-0262) |
| O-03 | Bookmarks | Navigate, add, rename, delete, reorder; persists | T-0255 | open | |
| O-04 | Password-protected PDFs | Prompt, open, honour permissions, save keeps encryption | T-0222, T-0238, T-0263 | open | |
| O-05 | Form filling | Fill, tab order, save with appearances, FDF/XFDF, flatten | T-0243 | open | |
| O-06 | Redaction | Content is removed, verified by re-extraction and a byte scan | T-0237, T-0257, T-0263 | open | |
| O-07 | No dead controls | Automated test: every enabled control emits a working command | T-0212 | open | docs/evidence/hardening/T-0212.md |
