//! Fresh filesystem capture across the eager-read threshold. Every iteration
//! verifies canonical tree identity and reads back every payload outside timing.
mod bench_util;
use casita::{
    experimental::{
        BlobId, BlobStore, Directory, MemoryBlobStore, MemoryMetadataStore, MetadataStore, Node,
        ObjectKey, PathComponent, Repository, RootName,
    },
    import::FilesystemImport,
};
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use std::time::{Duration, Instant};

fn filesystem_import(c: &mut Criterion) {
    let rt = bench_util::runtime();
    let mut group = c.benchmark_group("filesystem_import");
    group.sample_size(10);
    for size in [
        0,
        1024,
        256 * 1024 - 1,
        256 * 1024,
        256 * 1024 + 1,
        1024 * 1024,
    ] {
        let count = if size <= 1024 { 48 } else { 16 };
        let temp = tempfile::tempdir().unwrap();
        let mut directory = Directory::new();
        let mut files = Vec::new();
        for index in 0..count {
            let bytes = bench_util::random_bytes(index as u64 + 97, size);
            let name = format!("file-{index:04}");
            let path = temp.path().join(&name);
            std::fs::write(&path, &bytes).unwrap();
            let executable = cfg!(unix) && index % 3 == 0;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    &path,
                    std::fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }),
                )
                .unwrap();
            }
            directory
                .add(
                    PathComponent::try_from(name.as_str()).unwrap(),
                    Node::File {
                        digest: BlobId::new(blake3::hash(&bytes).into()),
                        size: size as u64,
                        executable,
                    },
                )
                .unwrap();
            files.push(bytes);
        }
        let expected = ObjectKey::directory(directory.digest());
        group.throughput(Throughput::Bytes((size * count) as u64));
        for concurrency in [1, 16] {
            group.bench_function(
                BenchmarkId::new(format!("bytes-{size}-files-{count}"), concurrency),
                |b| {
                    b.iter_custom(|iterations| {
                        rt.block_on(async {
                            let mut elapsed = Duration::ZERO;
                            for _ in 0..iterations {
                                let repository = Repository::new(
                                    MemoryBlobStore::new(),
                                    MemoryMetadataStore::new().unwrap(),
                                );
                                let root = RootName::try_from("benchmark/filesystem").unwrap();
                                let request = FilesystemImport::new(temp.path(), root.clone())
                                    .with_file_concurrency(
                                        std::num::NonZeroUsize::new(concurrency).unwrap(),
                                    );
                                let start = Instant::now();
                                let key = repository.import(request).await.unwrap();
                                elapsed += start.elapsed();
                                assert_eq!(key, expected);
                                assert_eq!(
                                    repository
                                        .metadata()
                                        .snapshot()
                                        .await
                                        .unwrap()
                                        .root(&root)
                                        .await
                                        .unwrap(),
                                    Some(expected.clone())
                                );
                                for bytes in &files {
                                    let id = BlobId::new(blake3::hash(bytes).into());
                                    assert_eq!(
                                        repository
                                            .payloads()
                                            .read_to_vec(&id)
                                            .await
                                            .unwrap()
                                            .unwrap(),
                                        *bytes
                                    );
                                }
                            }
                            elapsed
                        })
                    });
                },
            );
        }
    }
    group.finish();
}
criterion_group!(benches, filesystem_import);
criterion_main!(benches);
