# yrs 0.28.0, patched

This is yrs 0.28.0 (https://github.com/y-crdt/y-crdt, MIT license — see the
upstream repository's LICENSE) with fixes for panics found by fuzzing
coolpsonotes (`fuzz/`, target `crdt`). A panic in the browser build aborts the
whole app, and CRDT updates come from other people, so any vault member (or a
recipient with editing rights) could crash everyone's tabs with a crafted
update.

Wired in through `[patch.crates-io]` in the workspace `Cargo.toml` (and
`fuzz/Cargo.toml`). Every change is marked `coolpsonotes patch` in the source:

* `block_store.rs` `ClientBlockList::find_index`: plain binary search (the
  original divided by a block's end clock, which can be 0, and underflowed
  `mid - 1`).
* `block_store.rs` `BlockStore::push`: a block filling a gap is integrated
  only when a skip covers it entirely (the original unwrapped the lookup and
  subtracted clocks without checks).

Drop this directory once upstream releases equivalent fixes (and report them
upstream).
