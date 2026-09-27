//! Cold raw intake, first native receipt, warm/reopened reuse, and explicit audit.
use casita::nar::{NarHashAlgorithm, NarHashMethod, NarRequirements, ensure_nar, scrub_nar};
use casita::{
    MetadataChange, Repository,
    import::{FilesystemNarImport, NarImport},
};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use sha2::{Digest, Sha256, Sha512};
use std::time::{Duration, Instant};

#[path = "../../../benchmarks/fixtures/nar_import.rs"]
#[allow(dead_code)]
mod fixture;

/// Steady-state costs of capturing and comparing the invalidation generation.
/// Both revisions run this identical fixture through the public repository API.
fn generation(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut group = c.benchmark_group("nar_generation");
    group
        .sample_size(20)
        .warm_up_time(Duration::from_millis(200))
        .measurement_time(Duration::from_secs(2));
    for (backend, files) in [("local", 1), ("local", 256), ("memory", 1)] {
        let bytes = fixture::archive(files, 1024);
        let hash = Sha256::digest(&bytes);
        let data = tempfile::tempdir().unwrap();
        let repo = if backend == "local" {
            runtime.block_on(Repository::local(data.path())).unwrap()
        } else {
            Repository::memory().unwrap()
        };
        let report = runtime
            .block_on(repo.import(NarImport::new(bytes.as_slice())))
            .unwrap();
        let second =
            (backend == "local").then(|| runtime.block_on(Repository::local(data.path())).unwrap());
        let second_reader = second
            .as_ref()
            .map(|repo| runtime.block_on(repo.retained_reader()).unwrap());
        for operation in ["cached", "raw-dedup", "second-handle-cached", "scrub"] {
            if operation == "second-handle-cached" && second_reader.is_none() {
                continue;
            }
            let reader = if operation == "second-handle-cached" {
                second_reader.as_ref().unwrap()
            } else {
                report.reader()
            };
            // Establish the same warm availability state before either binary
            // is timed. Opening repositories and acquiring holds are excluded.
            runtime
                .block_on(ensure_nar(
                    reader,
                    report.root(),
                    &NarRequirements::default(),
                ))
                .unwrap();
            group.bench_function(
                BenchmarkId::new(format!("{backend}-{operation}"), files),
                |b| {
                    b.iter_custom(|count| {
                        runtime.block_on(async {
                            let mut elapsed = Duration::ZERO;
                            for _ in 0..count {
                                let start = Instant::now();
                                let measured = if operation == "raw-dedup" {
                                    repo.import(NarImport::new(bytes.as_slice())).await.unwrap()
                                } else if operation == "scrub" {
                                    scrub_nar(reader, report.root(), &NarRequirements::default())
                                        .await
                                        .unwrap()
                                } else {
                                    ensure_nar(reader, report.root(), &NarRequirements::default())
                                        .await
                                        .unwrap()
                                };
                                elapsed += start.elapsed();
                                assert_eq!(measured.nar_sha256(), hash.as_slice());
                                assert_eq!(measured.nar_size(), bytes.len() as u64);
                                assert_eq!(
                                    measured.stats().encoding_passes,
                                    u64::from(operation == "scrub")
                                );
                                assert_eq!(
                                    measured.stats().hash_payload_bytes,
                                    if matches!(operation, "raw-dedup" | "scrub") {
                                        (files * 1024) as u64
                                    } else {
                                        0
                                    }
                                );
                                if !matches!(operation, "raw-dedup" | "scrub") {
                                    assert!(measured.stats().association_hit);
                                }
                            }
                            elapsed
                        })
                    });
                },
            );
            // Every case also verifies the physical representation, outside
            // timing, rather than checking only its cached association.
            let scrub = runtime
                .block_on(scrub_nar(
                    reader,
                    report.root(),
                    &NarRequirements::default(),
                ))
                .unwrap();
            assert_eq!(scrub.nar_sha256(), hash.as_slice());
            assert_eq!(scrub.nar_size(), bytes.len() as u64);
        }
    }
    group.finish();
}

fn partial(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut group = c.benchmark_group("nar_partial");
    group
        .sample_size(10)
        .warm_up_time(Duration::from_millis(100))
        .measurement_time(Duration::from_millis(500));
    for backend in ["local", "memory"] {
        for files in [1, 256] {
            let bytes = fixture::archive(files, 1024);
            let sha256 = Sha256::digest(&bytes);
            let sha512 = Sha512::digest(&bytes);
            let requirements =
                NarRequirements::default().hash(NarHashMethod::Nar, NarHashAlgorithm::Sha512);
            group.bench_function(BenchmarkId::new(backend, files), |b| {
                b.iter_custom(|count| {
                    runtime.block_on(async {
                        let mut elapsed = Duration::ZERO;
                        for _ in 0..count {
                            // Each sample starts with SHA-256 only. Reusing this
                            // repository would turn later iterations into full hits.
                            let data = tempfile::tempdir().unwrap();
                            let repo = if backend == "local" {
                                Repository::local(data.path()).await.unwrap()
                            } else {
                                Repository::memory().unwrap()
                            };
                            let imported =
                                repo.import(NarImport::new(bytes.as_slice())).await.unwrap();
                            assert!(
                                imported
                                    .hash(NarHashMethod::Nar, NarHashAlgorithm::Sha512)
                                    .is_none()
                            );
                            let warm = ensure_nar(
                                imported.reader(),
                                imported.root(),
                                &NarRequirements::default(),
                            )
                            .await
                            .unwrap();
                            assert!(warm.stats().association_hit);
                            let start = Instant::now();
                            let measured =
                                ensure_nar(imported.reader(), imported.root(), &requirements)
                                    .await
                                    .unwrap();
                            elapsed += start.elapsed();
                            assert_eq!(measured.nar_sha256(), sha256.as_slice());
                            assert_eq!(measured.nar_size(), bytes.len() as u64);
                            assert_eq!(
                                measured
                                    .hash(NarHashMethod::Nar, NarHashAlgorithm::Sha512)
                                    .unwrap(),
                                sha512.as_slice()
                            );
                            assert!(!measured.stats().association_hit);
                            assert_eq!(measured.stats().encoding_passes, 1);
                            assert_eq!(measured.stats().hash_payload_bytes, (files * 1024) as u64);
                            let cached =
                                ensure_nar(imported.reader(), imported.root(), &requirements)
                                    .await
                                    .unwrap();
                            assert!(cached.stats().association_hit);
                            assert_eq!(cached.stats().hash_payload_bytes, 0);
                            assert_eq!(
                                cached
                                    .hash(NarHashMethod::Nar, NarHashAlgorithm::Sha512)
                                    .unwrap(),
                                sha512.as_slice()
                            );
                            let scrub =
                                scrub_nar(imported.reader(), imported.root(), &requirements)
                                    .await
                                    .unwrap();
                            assert_eq!(
                                scrub
                                    .hash(NarHashMethod::Nar, NarHashAlgorithm::Sha512)
                                    .unwrap(),
                                sha512.as_slice()
                            );
                        }
                        elapsed
                    })
                });
            });
        }
    }
    group.finish();
}

fn associations(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let source = tempfile::tempdir().unwrap();
    std::fs::write(source.path().join("hello"), b"hello\n").unwrap();
    let data = tempfile::tempdir().unwrap();
    let repo = runtime.block_on(Repository::local(data.path())).unwrap();
    let report = runtime
        .block_on(repo.import(FilesystemNarImport::new(source.path())))
        .unwrap();
    let hash = report.nar_sha256().to_vec();
    let casita::Node::Directory { digest, .. } = report.root() else {
        panic!("directory")
    };
    runtime
        .block_on(repo.commit(
            vec![],
            vec![MetadataChange::SetRoot {
                name: "benchmark".try_into().unwrap(),
                target: casita::ObjectKey::directory(*digest),
            }],
        ))
        .unwrap();
    let mut group = c.benchmark_group("nar_associations");
    group
        .sample_size(10)
        .warm_up_time(Duration::from_millis(100))
        .measurement_time(Duration::from_secs(1));
    for kind in ["warm", "scrub", "reopen", "native-warm"] {
        group.bench_function(kind, |b| {
            b.iter_custom(|count| {
                runtime.block_on(async {
                    let mut elapsed = Duration::ZERO;
                    for _ in 0..count {
                        let start = Instant::now();
                        let measured = match kind {
                            "scrub" => scrub_nar(
                                report.reader(),
                                report.root(),
                                &NarRequirements::default(),
                            )
                            .await
                            .unwrap(),
                            "native-warm" => repo
                                .import(FilesystemNarImport::new(source.path()))
                                .await
                                .unwrap(),
                            "reopen" => {
                                let reopened = Repository::local(data.path()).await.unwrap();
                                let reader = reopened.retained_reader().await.unwrap();
                                ensure_nar(&reader, report.root(), &NarRequirements::default())
                                    .await
                                    .unwrap()
                            }
                            _ => ensure_nar(
                                report.reader(),
                                report.root(),
                                &NarRequirements::default(),
                            )
                            .await
                            .unwrap(),
                        };
                        elapsed += start.elapsed();
                        assert_eq!(measured.nar_sha256(), hash);
                        assert_eq!(measured.stats().encoding_passes, u64::from(kind == "scrub"));
                        assert_eq!(
                            measured.stats().hash_payload_bytes,
                            if kind == "scrub" { 6 } else { 0 }
                        );
                    }
                    elapsed
                })
            })
        });
    }
    group.bench_function("raw", |b| {
        b.iter_custom(|count| {
            runtime.block_on(async {
                let mut elapsed = Duration::ZERO;
                for _ in 0..count {
                    let repo = Repository::memory().unwrap();
                    let start = Instant::now();
                    let measured = repo
                        .import(NarImport::new(
                            include_bytes!("../tests/fixtures/nar/hello.nar").as_slice(),
                        ))
                        .await
                        .unwrap();
                    elapsed += start.elapsed();
                    assert_eq!(measured.stats().encoding_passes, 0);
                    assert_eq!(measured.stats().hash_payload_bytes, 6);
                    assert_eq!(measured.nar_size(), 120);
                    assert_eq!(
                        measured.nar_sha256(),
                        &[
                            0x1c, 0x37, 0xd0, 0x1a, 0xf4, 0x0b, 0xe2, 0xe8, 0x06, 0x91, 0xde, 0x3c,
                            0xc3, 0xdf, 0x44, 0x37, 0x7a, 0x69, 0x9a, 0xfb, 0xb1, 0x7c, 0x68, 0xf0,
                            0x80, 0x96, 0x4b, 0x2f, 0xd0, 0x71, 0xfc, 0x13,
                        ]
                    );
                }
                elapsed
            })
        })
    });
    group.bench_function("native-cold", |b| {
        b.iter_custom(|count| {
            runtime.block_on(async {
                let mut elapsed = Duration::ZERO;
                for _ in 0..count {
                    let repo = Repository::memory().unwrap();
                    let start = Instant::now();
                    let measured = repo
                        .import(FilesystemNarImport::new(source.path()))
                        .await
                        .unwrap();
                    elapsed += start.elapsed();
                    assert_eq!(measured.nar_sha256(), hash);
                    assert_eq!(measured.stats().encoding_passes, 1);
                    assert_eq!(measured.stats().hash_payload_bytes, 6);
                }
                elapsed
            })
        })
    });
    group.finish();
}
criterion_group!(benches, associations, generation, partial);
criterion_main!(benches);
