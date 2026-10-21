# yrs 0.28.0, patched

This is yrs 0.28.0 (https://github.com/y-crdt/y-crdt, MIT license — see the
upstream repository's LICENSE) with fixes for panics found by fuzzing
notekpr (`fuzz/`, target `crdt`). A panic in the browser build aborts the
whole app, and CRDT updates come from other people, so any vault member (or a
recipient with editing rights) could crash everyone's tabs with a crafted
update.

Wired in through `[patch.crates-io]` in the workspace `Cargo.toml` (and
`fuzz/Cargo.toml`). Every change is marked `notekpr patch` in the source:

* `block_store.rs` `ClientBlockList::find_index`: plain binary search (the
  original divided by a block's end clock, which can be 0, and underflowed
  `mid - 1`).
* `block_store.rs` `BlockStore::push`: a block filling a gap is integrated
  only when a skip covers it entirely (the original unwrapped the lookup and
  subtracted clocks without checks).
* `update.rs` `Update::decode` and `id_set.rs` `Range::decode`: refuse what
  breaks the store's invariants later (and panicked while committing or
  encoding): a client listed twice, empty skip/GC blocks, clocks or delete
  ranges past `u32::MAX`.
* Decoders (`update.rs`, `any.rs`, `block.rs`, `state_vector.rs`): reserve at
  most `encoding::read::PREALLOC_LIMIT` elements up front for counts read
  from input (a fallible reservation still allocated and initialised
  gigabytes for a few bytes declaring a huge count); collections grow as
  elements are read.

Drop this directory once upstream releases equivalent fixes (and report them
upstream).
