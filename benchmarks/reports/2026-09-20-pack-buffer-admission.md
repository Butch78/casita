# Packed reads after the buffer budget stopped waiting

Packed reads share one byte budget for the compressed windows a read fetches
ahead of what its caller asked for. That budget used to be waited on, and a
reservation lives as long as the frames that slice into it, so a reader parked
mid-blob could hold windows that another reader was waiting for. Read-ahead
now runs only while the budget has room, and a read a caller is blocked on
takes whatever room is left, down to a single chunk fetched uncharged.

Nothing in that path waits any more, which is what makes it correct. The
question this run answers is what it costs, because the fetch path is what
serves every S3 read and the change could have turned read-ahead off under
ordinary load.

## Method

`benchmark run s3-fragmentation --profile standard` against a local rustfs
behind a TCP proxy adding 20 ms of round trip, reading whole files from the
prepared objects of 32 generations. Candidate is the commit that changed the
admission; baseline is the commit before it, built from the same tree with
only `blob/pack/fetch.rs`, `blob/pack.rs` and `byte_budget.rs` reverted, so
the two binaries differ in nothing else. Both variants read the same prepared
objects in the same run. One repetition, caches disabled, default and ample,
history layout.

## Result

Request counts and bytes read are identical in every cell. The 64 MiB random
corpus costs 39 range requests and 76,199,153 bytes cold for both binaries,
and the release executable costs 37 and 43,109,713. No read fell back to a
whole-pack fetch. Warm reads with an ample cache issue no requests at all for
either.

One cell differs, and only in bytes:

| Corpus | Cache | Phase | Requests | Baseline bytes | Candidate bytes |
|---|---|---|---:|---:|---:|
| random | default | warm | 2 / 2 | 419,172 | 479,583 |

The default cache is small enough to evict part of the working set, so a warm
read refetches a little of it. Both binaries issue the same two requests; the
candidate's cover 60 KB more, which is one range planned to a different edge
after a window was trimmed to the room the budget had. It is 0.08 percent of
the cold read those two requests follow.

Wall times move a few percent in both directions across the twelve cells,
which is the run to run spread of this benchmark and not a signal.

## Conclusion

Making read-ahead opportunistic costs nothing measurable on the path it was
most likely to hurt. The deferral it introduces is rare enough at this scale
not to show in request counts at all: on the rebuilt closure the serving side
deferred read-ahead 83 times in 11,148 range requests and never had to fetch
uncharged.

## Reproduce

```
cargo test --release --features cli,s3 --lib --no-run --message-format=json
benchmark run s3-fragmentation --no-build --profile standard \
    --probe-binary PROBE --baseline-probe-binary BASELINE \
    --rtt-ms 20 --caches disabled default ample --layouts history \
    --repetitions 1
```

`2026-09-20-pack-buffer-admission.json` is the raw result.
