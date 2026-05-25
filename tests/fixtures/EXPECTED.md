# Expected coroutine sizes — `known_stack_sizes.rs`

This table is the ground truth for validating goal (1) of the analyzer:
the **stack size of futures**. Each entry corresponds to one `pub async fn`
in `known_stack_sizes.rs` and gives:

- **Lower bound**: the minimum size implied by the variables that must live
  across an await point. This is the strict floor — the analyzer must not
  report a value below it.
- **Predicted size**: the size you'd get from a naive model
  `max(variant payloads) + 1 discriminant byte`, assuming no niche packing
  and no alignment padding (all payloads are `u8`-aligned by construction).
- **Notes**: known sources of variance vs. the predicted size.

## Layout model

Rust lowers an `async fn` body to a state-machine `enum` (a coroutine).
The variants are:

- `Unresumed` — initial state, holds the captured upvars
- `Returned` — terminal state after `Poll::Ready`
- `Panicked` — terminal state after a panic unwind
- One `Suspended-at-yield-K` variant per `.await` in the function body

The layout is `max(sizeof(variant)) + sizeof(discriminant) + padding`.
For our fixtures all locals are `u8`-aligned so padding is 0, and the
discriminant fits in 1 byte (≤ 256 yield points).

The compiler is permitted to:

1. **Niche-pack** the discriminant into an unused bit pattern inside a
   variant payload (e.g., a `bool` only uses 2 of its 256 values, so the
   discriminant can ride along in those niches). This can shave 1 byte
   off small coroutines.
2. **Overlap** locals from disjoint live ranges within one variant.
3. **Drop** locals whose final use is provably before any yield.

Where any of these affect the prediction, the **Notes** column flags it.

## Table

All predictions confirmed exact against `rustc -Zprint-type-sizes` on the
pinned toolchain (`nightly-2025-08-02`):

| # | Fixture                       | Lower bound | Predicted | Observed | Notes |
|---|-------------------------------|-------------|-----------|----------|-------|
| 1 | `empty`                       | 1           | 1         | 1        | discriminant only; no yield variants |
| 2 | `locals_no_await`             | 1           | 1         | 1        | `[u8; 128]` local never crosses a yield |
| 3 | `await_one_byte`              | 1           | 2         | 2        | no niche-pack observed |
| 4 | `await_257`                   | 257         | 258       | 258      | dominant: `PendOnce<256>` payload |
| 5 | `data_across_await`           | 1025        | 1026      | 1026     | `[u8; 1024]` + `PendOnce<0>` live across the await |
| 6 | `data_dropped_before_await`   | 1           | 2         | 2        | inner block ends before await; same shape as #3 |
| 7 | `sequential_awaits`           | 257         | 258       | 258      | only one inner future alive at a time |
| 8a | `nested_inner_small`         | 1           | 2         | 2        | same as #3 |
| 8b | `nested_outer_small`         | 2           | 3         | 3        | holds inner coroutine across its await |
| 9a | `nested_inner_big`           | 257         | 258       | 258      | same as #4 |
| 9b | `nested_outer_big`           | 258         | 259       | 259      | holds inner coroutine across its await |
| 10 | `two_live_across_await`      | 1025        | 1026      | 1026     | two 512-byte locals both live across one await |

## Validation rule of thumb

For each fixture, the analyzer-reported size `S` should satisfy:

```
lower_bound  <=  S  <=  predicted + small_slack
```

where `small_slack` accounts for at most a few bytes of unforeseen
alignment / state-tag overhead. Values strictly below the lower bound
indicate the analyzer is undercounting (a bug); values far above the
prediction indicate the analyzer is overcounting (also a bug, or the
compiler is making conservative layout choices we should investigate).

## How to run

```sh
cargo build --release
PROJECT_ANALYZER_MODE=default \
  ./target/release/project-analyzer \
  --edition 2021 \
  --crate-type bin \
  tests/fixtures/known_stack_sizes.rs
```

Note: the default `ProjectAnalyzer` mode currently filters out coroutines
smaller than 1000 bytes (see `src/main.rs`). To inspect the small
fixtures, lower that threshold (or use `async-graph` mode which doesn't
filter, though it reports call-graph depth/breadth rather than byte size).
