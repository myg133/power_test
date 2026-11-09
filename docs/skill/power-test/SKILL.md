---
name: power-test
description: |
  DEPRECATED. This skill was split into three on 2026-08-08.
  Do not load this for new requests. Use one of:

  - `power-test-run`     — single-endpoint stress test
  - `power-test-compare` — two-run side-by-side diff
  - `power-test-onboard` — 3-report SLA workflow (upstream + ours)

  The references/ directory is kept here as an archival copy
  of the pre-split content; for current work, see the three
  skills above.
---

# power-test (DEPRECATED — split into 3 skills)

This skill was a single SKILL.md covering single-endpoint
stress test, two-run compare, and the 3-report SLA workflow
together. On 2026-08-08 it was split into three more
focused skills so each one maps to a single task and the
Mavis router picks the right one without ambiguity:

- **`power-test-run`** — stress test ONE endpoint, get a
  single report. Use when the user says "压测"、"stress
  test"、single-endpoint benchmark.
- **`power-test-compare`** — given two run_ids, render the
  side-by-side diff (HTML or text). Use when the user has
  two runs and wants a delta, OR for any cross-target
  compare.
- **`power-test-onboard`** — the 3-report SLA workflow for
  bringing a new upstream model online: vendor + ours +
  diff, with TPM / RPM extraction and hand-off
  checklists.

The references/ directory below is kept as an archival
copy of the pre-split content. For current work, see the
three skills above.
