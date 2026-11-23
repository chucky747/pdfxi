# T-0260B real-corpus save budget blocker

- Task: T-0260B (qa) - open, edit and save budgets on the real corpus
- Recorded: 2026-11-21
- Status: **resolved 2026-11-23** - `docs/TESTING.md` now gives real-file saves
  a 25 ms durability floor; the measurement records 0 derived-budget breaches.
  The two original breaches and their samples stay on record in
  `docs/evidence/acceptance/T-0260B.md` section 3.
- Current as of 0.1.4: the deterministic in-memory harness
  (`cargo xtask bench --report`) passes 18/18 metrics with 0 breaches and 0
  regressions.
- Evidence: `docs/evidence/acceptance/T-0260B.md` (sections 3 and 4)

## Summary

Open and one incremental annotation edit pass their derived and absolute
budgets on every committed real-world fixture. Two **save** cases are over the
derived budget. Both use under 1% of the documented absolute budget, and neither
budget was weakened to make them pass.

## The two breaches

Budget rule (documented in `docs/TESTING.md`, applied unchanged):
`max(base * pages / 500, 5 ms) * 1.25`, with `base = 2000 ms` for the
incremental annotation save. A 1-page fixture therefore gets a 6.25 ms derived
budget and a 2-page fixture 10.0 ms. The tracked value is the lowest median of
five independent rounds (7 measured iterations each, 2 warm-up), which is more
forgiving than the documented best-of-3 rule.

| Case | Tracked median | Derived budget | Over by | Absolute budget | Share of absolute |
|---|---:|---:|---:|---:|---:|
| `fixtures/realworld/browser-print.pdf` save, 1 page, 51,696 B | **15.1668 ms** | 6.25 ms | 2.43x | 2000 ms | 0.76% |
| `fixtures/realworld/microsoft-excel-2019-sample.pdf` save, 2 pages, 92,366 B | **10.4397 ms** | 10.0 ms | 1.04x | 2000 ms | 0.52% |

Per-round medians (ms):

```text
browser-print.pdf save : 15.1668  16.4499  15.2699  15.3145  16.1221
excel-sample.pdf save  : 11.0271  11.8371  11.1676  10.4397  11.4018
forms-selfmade.pdf save:  3.9376   3.4603   4.1326   4.0249   3.9848   (pass, 6.25 budget)
powerpoint-slides save : 12.5933   9.1885  12.3540  12.6415  12.1829   (pass, 15.0 budget)
```

## Likely cause

The derived floor is below this host's file-durability latency; the save work
itself is not large.

1. `crates/pdfxi-document/src/atomic.rs` writes a sibling temp file, calls
   `sync_all` (flush to device), then renames. Save latency is therefore a
   filesystem flush, not a parse/write cost.
2. The save medians are not ordered by document size: 51,696 B
   (`browser-print`) is slower than 119,341 B (`powerpoint-slides`), and
   1,195 B (`forms`) is 4x faster than everything else. Size is not the driver.
3. The harness `save` metric, which never touches a filesystem, is
   0.0140-3.2954 ms on the same host. The difference is durability.
4. The 5 ms floor was calibrated on the harness's in-memory synthetic fixtures.
   A 1-page save on this host ranged 1.86-27.78 ms across observed runs (forms
   1.86-7.36 ms, browser-print 12.06-27.78 ms), straddling the 6.25 ms floor by
   up to 4.4x.

## Not the cause (checked)

- Not the parser or incremental writer: `open` on the same files is
  0.1269-0.6685 ms and `edit` is 0.0360-0.5661 ms, all far inside budget.
- Not a single bad run: `browser-print` save breached in all 10 observed runs
  (12.06-27.78 ms); `excel` save breached in the five tracked rounds and in
  loaded runs, and passed in two lighter calibration rounds (7.99, 8.25 ms).
  Full inventory: `docs/evidence/acceptance/T-0260B.md` section 3.
- Not a fixed cost of the edit path: the same sessions edit in 0.036-0.567 ms
  in quiescent rounds (and under 1.07 ms when the host is loaded), so the
  incremental append itself is fast.
- Not concurrent build load: one calibration run during another lane's compile
  was slower for every save, but the five recorded rounds were quiescent.

## Resolution (2026-11-23)

The owner of `docs/TESTING.md` chose the disk-flush-aware floor:

- real-file saves use a **25 ms floor** before the +25% allowance, giving
  31.25 ms at one page; the 2000 ms absolute row is unchanged;
- the in-memory harness `save` metric keeps its 5 ms floor and 6.25 ms small
  budget, because it never calls `sync_all`;
- `docs/TESTING.md` documents both timed regions, and
  `docs/evidence/acceptance/T-0260B-measure.rs` applies the floor only to the
  `save` case.

Do not raise the 2000 ms absolute budget: it is met with 100x margin.

## Repro

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File docs\evidence\acceptance\T-0260B-measure.ps1
# before the policy change: exits 1; "summary cases=15 derived_budget_breaches=1..4 unsupported=1"
# after the policy change: exits 0; the same cases report 0 derived-budget breaches
```

Save output goes to `target/tmp/t0260b-save-r*`; committed fixtures are read
only. No app, core, xtask or matrix change is requested by this blocker.
