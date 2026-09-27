//! Production Git pack generation across the buffered/streaming boundary.
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use casita::experimental::*;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use futures::Future;

type Service = GitFetchService<MemoryBlobStore, MemoryMetadataStore>;

async fn fixture(body: &[u8]) -> (Service, GitFetchRequest) {
    let repository = Repository::new(MemoryBlobStore::new(), MemoryMetadataStore::new().unwrap());
    let blob = git_object_key_for_body(GitObjectFormat::Sha1, GitObjectKind::Blob, body).unwrap();
    let mutation = repository.mutation_session().await.unwrap();
    let staged = mutation.stage_object(blob.clone(), body).await.unwrap();
    mutation.publish_unrooted(vec![staged]).await.unwrap();
    drop(mutation);
    // A lightweight tag can point directly to a blob. One object makes pack
    // validation independent of Casita's decoder and isolates the boundary.
    publish_git_view(
        &repository,
        "fairness",
        &GitViewBody {
            object_format: GitObjectFormat::Sha1,
            refs: BTreeMap::from([(
                CanonicalRefName::try_from("refs/tags/data").unwrap(),
                GitRefValue::Direct(blob.clone()),
            )]),
            default_ref: None,
            pack: None,
            objects: BTreeSet::from([blob.clone()]),
        },
    )
    .await
    .unwrap();
    let service = GitFetchService::bind(&repository, "fairness", GitFetchLimits::default())
        .await
        .unwrap();
    let request = GitFetchRequest {
        wants: vec![blob.native_id().to_vec()],
        haves: vec![],
        depth: None,
        done: true,
        multi_ack_detailed: false,
        side_band_64k: false,
    };
    (service, request)
}

fn validate(pack: &GitFetchPack, body: &[u8]) {
    assert!(pack.shallow.is_empty());
    let bytes = &pack.pack;
    assert_eq!(&bytes[..12], b"PACK\0\0\0\x02\0\0\0\x01");
    let end = bytes.len() - 20;
    let mut hash = gix_hash::hasher(gix_hash::Kind::Sha1);
    hash.update(&bytes[..end]);
    assert_eq!(hash.try_finalize().unwrap().as_slice(), &bytes[end..]);
    let mut offset = 12;
    let first = bytes[offset];
    assert_eq!((first >> 4) & 7, 3); // native Git blob entry
    let mut size = (first & 15) as usize;
    let mut shift = 4;
    let mut byte = first;
    while byte & 128 != 0 {
        offset += 1;
        byte = bytes[offset];
        size |= ((byte & 127) as usize) << shift;
        shift += 7;
    }
    assert_eq!(size, body.len());
    let mut decoder = flate2::read::ZlibDecoder::new(&bytes[offset + 1..end]);
    let mut decoded = Vec::new();
    decoder.read_to_end(&mut decoded).unwrap();
    assert_eq!(decoder.total_in() as usize, end - offset - 1);
    assert_eq!(decoded, body);
}

async fn measure(
    service: &Service,
    request: &GitFetchRequest,
    body: &[u8],
    concurrency: usize,
    observe: bool,
) -> (Duration, Duration, Duration) {
    let done = Arc::new(AtomicBool::new(false));
    let observer = if observe {
        let done = done.clone();
        let (ready, started) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let mut previous = Instant::now();
            let mut gap = Duration::ZERO;
            ready.send(()).unwrap();
            while !done.load(Ordering::Relaxed) {
                tokio::task::yield_now().await;
                let now = Instant::now();
                gap = gap.max(now - previous);
                previous = now;
            }
            gap
        });
        started.await.unwrap();
        Some(task)
    } else {
        None
    };
    let start = Instant::now();
    let tasks: Vec<_> = (0..concurrency)
        .map(|_| {
            let service = service.clone();
            let request = request.clone();
            tokio::spawn(async move {
                let future = service.build_pack(&request);
                tokio::pin!(future);
                let mut longest_poll = Duration::ZERO;
                let pack = futures::future::poll_fn(|cx| {
                    let start = Instant::now();
                    let result = future.as_mut().poll(cx);
                    longest_poll = longest_poll.max(start.elapsed());
                    result
                })
                .await
                .unwrap();
                (pack, longest_poll)
            })
        })
        .collect();
    let results = futures::future::join_all(tasks).await;
    let elapsed = start.elapsed();
    done.store(true, Ordering::Relaxed);
    let gap = match observer {
        Some(task) => task.await.unwrap(),
        None => Duration::ZERO,
    };
    let mut longest_poll = Duration::ZERO;
    // Every output is checked, outside the measurement and observer interval.
    for result in results {
        let (pack, poll) = result.unwrap();
        validate(&pack, body);
        longest_poll = longest_poll.max(poll);
    }
    (elapsed, longest_poll, gap)
}

fn fairness(c: &mut Criterion) {
    let mut group = c.benchmark_group("git_fetch_fairness");
    group.sample_size(10);
    for workers in [1, 4] {
        let runtime = if workers == 1 {
            tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
        } else {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(workers)
                .build()
                .unwrap()
        };
        for size in [1_048_575, 1_048_576, 1_048_577, 4_194_304] {
            for corpus in ["text", "random"] {
                let mut body = vec![b'a'; size];
                if corpus == "random" {
                    blake3::Hasher::new()
                        .update(b"git-fetch-fairness-v1")
                        .finalize_xof()
                        .fill(&mut body);
                }
                let (service, request) = runtime.block_on(fixture(&body));
                for concurrency in [1, 4] {
                    // Warm up outside both diagnostics and Criterion timing.
                    runtime.block_on(measure(&service, &request, &body, concurrency, false));
                    for repetition in 0..3 {
                        let (elapsed, poll, gap) =
                            runtime.block_on(measure(&service, &request, &body, concurrency, true));
                        println!(
                            "{}",
                            serde_json::json!({
                                "benchmark": "git_fetch_fairness", "workers": workers,
                                "bytes": size, "corpus": corpus, "concurrency": concurrency,
                                "repetition": repetition, "elapsed_ns": elapsed.as_nanos(),
                                "longest_poll_ns": poll.as_nanos(), "ready_task_gap_ns": gap.as_nanos(),
                                "correctness": "passed"
                            })
                        );
                    }
                    group.throughput(Throughput::Bytes((size * concurrency) as u64));
                    group.bench_function(
                        BenchmarkId::new(format!("{workers}w/{corpus}/{concurrency}clients"), size),
                        |b| {
                            b.iter_custom(|iterations| {
                                (0..iterations)
                                    .map(|_| {
                                        runtime
                                            .block_on(measure(
                                                &service,
                                                &request,
                                                &body,
                                                concurrency,
                                                false,
                                            ))
                                            .0
                                    })
                                    .sum()
                            });
                        },
                    );
                }
            }
        }
    }
    group.finish();
}

criterion_group!(benches, fairness);
criterion_main!(benches);
