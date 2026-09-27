# Why online GC can finish without reclaiming garbage

The combined [online-holds benchmark](2026-09-08-online-holds.md) completed 12 GC
passes but left all obsolete objects for final cleanup. Focused tests distinguish
legitimate snapshot retention from an unnecessary mark conflict. The benchmark
alone did not record enough detail to apportion those effects numerically.

A snapshot hold retains objects born through its admitted generation, including
unrooted records. It does not retain only the object subsequently read from it.
The benchmark opens fresh snapshots repeatedly after imports have created
garbage. Those snapshots can therefore protect the garbage even though the
reader only opens the sentinel payload. Released pins also remain as liveness
history until their collector pass finishes.

The first deterministic test keeps an old reader and an idle writer alive for
four rounds. In each round it publishes a new unrooted blob, opens a fresh
snapshot, and runs GC. GC removes zero objects while that fresh snapshot is
held. After releasing it, GC removes exactly one logical object and its payload,
while the original reader and idle writer remain alive. The reader's sentinel
bytes remain correct throughout. Thus, holds do not globally block collection;
the generation at which a snapshot was acquired matters.

The second test admits 32 entirely empty writers between GC marking and
execution. Before the fix, execution deterministically returned
`Busy("payload pins changed during collection mark")`. None of those writers
protected any data, so invalidating the mark was unnecessary. Liveness comparison
now ignores staging pins only when both their catalog and resource set are
empty. The same test now removes the unreachable object and payload while all
32 writers remain alive. If one writer protects that object after marking,
execution still returns Busy and preserves the payload.

Additional checks cover memory, file, and conditional object-store ledgers.
Snapshot and catalog-only pins still affect liveness. An empty writer cannot
acquire its first resource while the prune fence is active; after the fence is
released, protection succeeds and changes the liveness comparison. Retiring that
pin keeps the protection visible until collector completion.

This removes one avoidable conflict, not every cause of a Busy result. Exact
revision checks at admission, real changes in protected data, and metadata
publication conflicts remain. Nor does it make garbage eligible while a fresh
snapshot legitimately retains it. A future experiment should compare fresh
snapshot readers with readers that pin only the requested object closure;
weakening snapshot retention would change the stable-read contract.
The subsequent [object-scoped read change](2026-09-08-scoped-reads.md) implements
that narrower read path and records a benchmark rerun.

Validation: the empty-writer regression failed before the change and passed
afterward. All 169 selected library tests (32 pin-ledger, 74 repository, 63
packed-storage), five online-GC/cancellation integration tests, and
all-features/all-targets Clippy with warnings denied passed. Formatting and diff
checks passed. No timing improvement is claimed from these deterministic tests.
