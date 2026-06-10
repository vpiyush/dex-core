# Published performance captures

One directory per **run id**: the git short sha that produced the numbers,
suffixed `-dirty` if tracked files were modified at capture time (the
timestamp is the fallback off-git). Each directory is the canonical, committed
record of one `cargo bench-all capture` on a prepped machine. The top-level
README quotes these numbers and links these curves.

## Scratch runs vs published captures

The same benchkit artifacts live two different lives. Do not confuse them.

| | `target/bench-runs/<run id>/` | `docs/perf/<run id>/` |
|---|---|---|
| written by | every `cargo bench` / `cargo bench-all` | only `cargo bench-all capture` |
| purpose | iteration, `--compare` A/B baselines | the published record |
| machine state | whatever the dev box was doing | prepped (pinned clock, siblings offline, quiet) |
| extras | reports only | + ENVIRONMENT.txt, per-pass `.txt`, rendered SVGs |
| lifetime | dies with `cargo clean` | committed to git |

Identity lives in the directory name, never in filenames. A rerun at the same
run id overwrites in place: one commit, one canonical set of numbers. The
timestamp is provenance, not identity — it is stamped inside every report
header and in ENVIRONMENT.txt.

## Clock policy

- **peak** — turbo on, top bin requested; the hardware grants what thermals
  allow, and the granted rate is measured and stamped as `clock_eff`. Best
  numbers the machine can produce; what the README quotes.
- **pinned** — min = max at nominal (2.8 GHz here); identical clock run to
  run. Slower, but the only honest basis for comparing two commits.

## Captures

| capture | date (UTC) | mode | clock_eff | notes |
|---|---|---|---|---|
| [2026-06-10](2026-06-10/) | 2026-06-10 | peak | 4.67 GHz | code `636f6ff-dirty`; predates run-id naming (date-keyed dir, sha-keyed files) |
