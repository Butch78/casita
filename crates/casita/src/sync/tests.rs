use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use futures::stream::BoxStream;
use tokio::sync::Notify;

use crate::test_util::pc;
use crate::{
    BlobId, BlobReader, BlobWriter, ChunkMeta, ChunkedBlobStore, CommitResult, Digest, Directory,
    FormatLimits, FormatRegistry, IpldCid, LinkedIpld, MemoryBlobStore, MemoryMetadataStore,
    MetadataMutation, MetadataSnapshot, Node, RAW_CODEC, RootChange, RootRecord,
};

fn repository() -> Repository<MemoryBlobStore, MemoryMetadataStore> {
    Repository::new(MemoryBlobStore::new(), MemoryMetadataStore::new().unwrap())
}

struct ChunkBatchProbe<'a> {
    inner: Box<dyn TransferReadSession + 'a>,
    active: AtomicUsize,
    peak: AtomicUsize,
    // Every in-flight chunk read waits here, so the observed peak is the
    // batch window itself rather than a race against a timer.
    gate: Option<Arc<tokio::sync::Barrier>>,
    fail: bool,
}

#[async_trait]
impl TransferReadSession for ChunkBatchProbe<'_> {
    fn revision(&self) -> RepositoryRevision {
        self.inner.revision()
    }
    async fn object(&self, key: &ObjectKey) -> Result<Option<ObjectRecord>, TransferError> {
        self.inner.object(key).await
    }
    async fn root(&self, name: &RootName) -> Result<Option<ObjectKey>, TransferError> {
        self.inner.root(name).await
    }
    async fn open_payload(
        &self,
        record: &ObjectRecord,
    ) -> Result<Option<TransferPayloadReader>, TransferError> {
        self.inner.open_payload(record).await
    }
    async fn chunks(&self, record: &ObjectRecord) -> Result<Option<Vec<ChunkMeta>>, TransferError> {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        let _guard = ActivePayloadOpen(&self.active);
        self.peak.fetch_max(active, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            gate.wait().await;
        }
        if self.fail {
            return Err(TransferError::SourceTransport("test failure".into()));
        }
        Ok((record.payload_size().is_multiple_of(2)).then(|| {
            vec![ChunkMeta {
                digest: crate::ChunkId::new([record.payload_size() as u8; 32].into()),
                size: record.payload_size(),
            }]
        }))
    }
}

#[tokio::test]
async fn chunk_batches_preserve_order_and_bound_cancellable_fallback() {
    let source = repository();
    let mutation = source.mutation_session().await.unwrap();
    let first = mutation.stage_blob(b"odd").await.unwrap();
    let second = mutation.stage_blob(b"even").await.unwrap();
    let first_key = first.record().key().clone();
    let second_key = second.record().key().clone();
    mutation
        .publish_unrooted(vec![first, second])
        .await
        .unwrap();
    let session = source
        .begin_transfer(TransferSelection::Snapshot)
        .await
        .unwrap();
    let first = session.object(&first_key).await.unwrap().unwrap();
    let second = session.object(&second_key).await.unwrap().unwrap();
    let records: Vec<_> = (0..96)
        .map(|i| {
            if i % 3 == 0 {
                second.clone()
            } else {
                first.clone()
            }
        })
        .collect();
    let mut probe = ChunkBatchProbe {
        inner: session,
        active: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
        gate: Some(Arc::new(tokio::sync::Barrier::new(32))),
        fail: false,
    };
    assert!(probe.chunks_batch(&[]).await.unwrap().is_empty());
    let maps = probe.chunks_batch(&records).await.unwrap();
    assert_eq!(maps.len(), records.len());
    for (i, map) in maps.iter().enumerate() {
        if i % 3 == 0 {
            assert_eq!(map.as_ref().unwrap()[0].size, 4);
        } else {
            assert!(map.is_none());
        }
    }
    assert_eq!(probe.peak.load(Ordering::SeqCst), 32);
    assert_eq!(probe.active.load(Ordering::SeqCst), 0);
    probe.gate = None;

    let missing = ObjectKey::blob(crate::BlobId::new([99; 32].into()));
    let keys = vec![second_key.clone(), missing, first_key.clone(), second_key];
    let combined = probe.objects_with_chunks(&keys).await.unwrap();
    assert_eq!(combined.len(), keys.len());
    assert!(combined[1].is_none());
    assert!(combined[2].as_ref().unwrap().chunks.is_none());
    for i in [0, 2, 3] {
        assert_eq!(combined[i].as_ref().unwrap().record.key(), &keys[i]);
    }
    assert_eq!(combined[0], combined[3]);
    assert_eq!(
        combined[0].as_ref().unwrap().chunks.as_ref().unwrap()[0].size,
        4
    );
    assert!(probe.objects_with_chunks(&[]).await.unwrap().is_empty());
    let keys: Vec<_> = records.iter().map(|record| record.key().clone()).collect();
    // One more participant than the window: the batch can never finish.
    probe.gate = Some(Arc::new(tokio::sync::Barrier::new(33)));
    let mut batch = Box::pin(probe.objects_with_chunks(&keys));
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::select! {
            _ = &mut batch => panic!("slow batch completed"),
            _ = async {
                while probe.active.load(Ordering::SeqCst) < 32 { tokio::task::yield_now().await; }
            } => {}
        }
    })
    .await
    .unwrap();
    drop(batch);
    assert_eq!(probe.active.load(Ordering::SeqCst), 0);
    probe.gate = None;
    probe.fail = true;
    assert!(probe.objects_with_chunks(&keys).await.is_err());
    assert_eq!(probe.active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn missing_selected_name_releases_acquisition_protection() {
    let source = repository();
    let mutation = source.mutation_session().await.unwrap();
    let garbage = mutation.stage_blob(b"unrelated").await.unwrap();
    mutation.publish_unrooted(vec![garbage]).await.unwrap();
    drop(mutation);
    let name = RootName::try_from("missing").unwrap();
    assert!(matches!(source.begin_transfer(TransferSelection::Selected {
        objects: Vec::new(), roots: vec![name.clone()],
    }).await, Err(TransferError::MissingSourceRoot(missing)) if missing == name));
    crate::flush_repository_leases().await.unwrap();
    assert_eq!(
        source.try_collect().await.unwrap().removed.logical_objects,
        1
    );
}

#[cfg(feature = "ssh")]
#[tokio::test]
async fn transfer_catalog_holds_preserve_historical_reads_until_release() {
    use tokio::io::AsyncReadExt;

    fn packs(root: &std::path::Path) -> BTreeSet<std::path::PathBuf> {
        fn walk(root: &std::path::Path, files: &mut BTreeSet<std::path::PathBuf>) {
            for entry in std::fs::read_dir(root).unwrap() {
                let entry = entry.unwrap();
                if entry.file_type().unwrap().is_dir() {
                    walk(&entry.path(), files);
                } else {
                    files.insert(entry.path());
                }
            }
        }
        let mut files = BTreeSet::new();
        walk(&root.join("blobs/packs"), &mut files);
        files
    }
    let mut payload = vec![0; 512 * 1024];
    blake3::Hasher::new()
        .update(b"selected")
        .finalize_xof()
        .fill(&mut payload);
    let mut garbage = vec![0; 512 * 1024];
    blake3::Hasher::new()
        .update(b"garbage")
        .finalize_xof()
        .fill(&mut garbage);
    for remote in [false, true] {
        for mixed in [false, true] {
            for snapshot in [false, true] {
                let temp = tempfile::tempdir().unwrap();
                let source = Repository::local_with_pack_options(
                    temp.path(),
                    crate::PackOptions {
                        target_size: u64::MAX,
                        cache_capacity: 0,
                    },
                )
                .await
                .unwrap();
                let mutation = source.mutation_session().await.unwrap();
                let staged = mutation.stage_blob(&payload).await.unwrap();
                let key = staged.record().key().clone();
                let mut objects = vec![staged];
                if mixed {
                    objects.push(mutation.stage_blob(&garbage).await.unwrap());
                }
                mutation.publish_unrooted(objects).await.unwrap();
                drop(mutation);
                let held_packs = packs(temp.path());
                assert_eq!(held_packs.len(), 1);
                if !mixed {
                    let mutation = source.mutation_session().await.unwrap();
                    let staged = mutation.stage_blob(&garbage).await.unwrap();
                    mutation.publish_unrooted(vec![staged]).await.unwrap();
                }
                crate::flush_repository_leases().await.unwrap();
                let all_packs = packs(temp.path());
                assert_eq!(all_packs.len(), if mixed { 1 } else { 2 });
                let selection = if snapshot {
                    TransferSelection::Snapshot
                } else {
                    TransferSelection::Selected {
                        objects: vec![key.clone()],
                        roots: Vec::new(),
                    }
                };
                let (session, server): (Box<dyn TransferReadSession + '_>, _) = if remote {
                    let (client, server) = tokio::io::duplex(4096);
                    let repository = source.clone();
                    let (input, output) = tokio::io::split(server);
                    let task = tokio::spawn(async move {
                        crate::sync::ssh::serve_transfer_stdio(&repository, input, output).await
                    });
                    let (input, output) = tokio::io::split(client);
                    (
                        Box::new(
                            crate::sync::ssh::connect_transfer_stdio_source(
                                input, output, selection,
                            )
                            .await
                            .unwrap(),
                        ),
                        Some(task),
                    )
                } else {
                    (source.begin_transfer(selection).await.unwrap(), None)
                };
                let record = session.object(&key).await.unwrap().unwrap();
                let mut expected_proof = Vec::new();
                session
                    .open_proof(&record)
                    .await
                    .unwrap()
                    .unwrap()
                    .read_to_end(&mut expected_proof)
                    .await
                    .unwrap();
                let mut reader = session.open_payload(&record).await.unwrap().unwrap();
                let mut received = vec![0];
                reader.read_exact(&mut received).await.unwrap();
                // An independent handle makes the source keep its historical
                // catalog and disables shared backend state as an escape hatch.
                let collector = Repository::local_with_pack_options(
                    temp.path(),
                    crate::PackOptions {
                        target_size: u64::MAX,
                        cache_capacity: 0,
                    },
                )
                .await
                .unwrap();
                crate::flush_repository_leases().await.unwrap();
                assert_eq!(
                    collector
                        .try_collect()
                        .await
                        .unwrap()
                        .removed
                        .logical_objects,
                    usize::from(!snapshot)
                );
                let after = packs(temp.path());
                assert!(
                    held_packs.is_subset(&after),
                    "historical selected pack must survive"
                );
                for path in all_packs.difference(&held_packs) {
                    assert!(
                        path.exists(),
                        "historical catalog must retain separate packs"
                    );
                }
                reader.read_to_end(&mut received).await.unwrap();
                assert_eq!(received, payload);
                drop(reader);
                let mut proof = session.open_proof(&record).await.unwrap().unwrap();
                let mut received = vec![0];
                proof.read_exact(&mut received).await.unwrap();
                collector.try_collect().await.unwrap();
                proof.read_to_end(&mut received).await.unwrap();
                assert_eq!(received, expected_proof);
                drop(proof);
                let destination = repository();
                transfer(
                    &HeldSession(session.as_ref()),
                    &destination,
                    TransferRequest {
                        objects: vec![ObjectRequest {
                            key: key.clone(),
                            recursive: true,
                        }],
                        roots: Vec::new(),
                    },
                    TransferOptions::default(),
                )
                .await
                .unwrap();
                let (_, mut copied) = destination.open_payload(&key).await.unwrap().unwrap();
                let mut received = Vec::new();
                copied.read_to_end(&mut received).await.unwrap();
                assert_eq!(received, payload);
                drop(session);
                if let Some(server) = server {
                    server.await.unwrap().unwrap();
                }
                crate::flush_repository_leases().await.unwrap();
                collector.try_collect().await.unwrap();
                assert!(packs(temp.path()).is_empty());
            }
        }
    }
}

#[cfg(feature = "ssh")]
#[tokio::test]
async fn local_and_ssh_selections_retain_exact_graphs_at_the_original_revision() {
    use tokio::io::AsyncReadExt;
    for (remote, whole_snapshot) in [(false, false), (false, true), (true, false), (true, true)] {
        let source = repository();
        let name = RootName::try_from("selected").unwrap();
        let mutation = source.mutation_session().await.unwrap();
        let named = mutation.stage_blob(b"named graph").await.unwrap();
        let exact = mutation.stage_blob(b"exact graph").await.unwrap();
        let garbage = mutation.stage_blob(b"unrelated garbage").await.unwrap();
        let named_key = named.record().key().clone();
        let exact_key = exact.record().key().clone();
        mutation
            .publish(
                vec![named, exact, garbage],
                vec![RootChange::Set {
                    name: name.clone(),
                    target: named_key.clone(),
                }],
            )
            .await
            .unwrap();
        drop(mutation);
        let selection = if whole_snapshot {
            TransferSelection::Snapshot
        } else {
            TransferSelection::Selected {
                objects: vec![exact_key.clone()],
                roots: vec![name.clone()],
            }
        };
        let (session, server): (Box<dyn TransferReadSession + '_>, _) = if remote {
            let (client, server) = tokio::io::duplex(4096);
            let server_source = source.clone();
            let (input, output) = tokio::io::split(server);
            let task = tokio::spawn(async move {
                crate::sync::ssh::serve_transfer_stdio(&server_source, input, output).await
            });
            let (input, output) = tokio::io::split(client);
            (
                Box::new(
                    crate::sync::ssh::connect_transfer_stdio_source(input, output, selection)
                        .await
                        .unwrap(),
                ),
                Some(task),
            )
        } else {
            (source.begin_transfer(selection).await.unwrap(), None)
        };
        let revision = session.revision();
        let mutation = source.mutation_session().await.unwrap();
        let replacement = mutation.stage_blob(b"replacement root").await.unwrap();
        let replacement_key = replacement.record().key().clone();
        mutation
            .publish_rooted(vec![replacement], name.clone(), replacement_key)
            .await
            .unwrap();
        drop(mutation);
        crate::flush_repository_leases().await.unwrap();
        assert_eq!(
            source.try_collect().await.unwrap().removed.logical_objects,
            if whole_snapshot { 0 } else { 1 }
        );
        assert_eq!(session.revision(), revision);
        assert_eq!(session.root(&name).await.unwrap(), Some(named_key.clone()));
        let destination = repository();
        transfer(
            &HeldSession(session.as_ref()),
            &destination,
            TransferRequest {
                objects: vec![
                    ObjectRequest {
                        key: named_key.clone(),
                        recursive: true,
                    },
                    ObjectRequest {
                        key: exact_key.clone(),
                        recursive: false,
                    },
                ],
                roots: Vec::new(),
            },
            TransferOptions::default(),
        )
        .await
        .unwrap();
        let record = session.object(&exact_key).await.unwrap().unwrap();
        let mut reader = session.open_payload(&record).await.unwrap().unwrap();
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(bytes, b"exact graph");
        drop(reader);
        drop(session);
        if let Some(server) = server {
            server.await.unwrap().unwrap();
        }
        crate::flush_repository_leases().await.unwrap();
        assert_eq!(
            source.try_collect().await.unwrap().removed.logical_objects,
            if whole_snapshot { 3 } else { 2 }
        );
    }
}

#[tokio::test]
async fn split_source_uses_metadata_revision_and_blob_endpoint_for_paths_and_roots() {
    let (original, name, root, _, leaf, _) = filesystem_source().await;
    // Metadata has no bytes; the payload repository has no logical records
    // or roots. Neither endpoint can supply a complete transfer alone.
    let metadata = Repository::new(MemoryBlobStore::new(), original.metadata().clone());
    let payloads = Repository::new(
        original.payloads().clone(),
        MemoryMetadataStore::new().unwrap(),
    );
    let metadata_session = metadata
        .begin_transfer(TransferSelection::Selected {
            objects: Vec::new(),
            roots: vec![name.clone()],
        })
        .await
        .unwrap();
    let revision = metadata_session.revision();
    let source = SplitTransferSession::new(
        metadata_session,
        payloads
            .begin_transfer(crate::sync::TransferSelection::Snapshot)
            .await
            .unwrap(),
    );
    assert_eq!(source.revision(), revision);
    assert_ne!(source.revision(), source.payloads.revision());
    assert_eq!(source.root(&name).await.unwrap(), Some(root.clone()));
    for discovery in [
        TransferDiscovery::Exhaustive,
        TransferDiscovery::ReuseVerified,
    ] {
        let destination = repository();
        let outcome = transfer_path(
            &HeldSession(&source),
            &destination,
            &name,
            "sub/selected.txt",
            Some(name.clone()),
            TransferOptions::default().with_discovery(discovery),
        )
        .await
        .unwrap();
        assert!(outcome.transfer.is_some());
        assert_eq!(
            destination
                .metadata()
                .snapshot()
                .await
                .unwrap()
                .root(&name)
                .await
                .unwrap(),
            Some(leaf.clone())
        );
        assert_eq!(
            destination
                .payloads()
                .read_to_vec(&BlobId::new(Digest::hash(b"selected")))
                .await
                .unwrap(),
            Some(b"selected".to_vec())
        );

        let request = TransferRequest {
            objects: vec![ObjectRequest {
                key: root.clone(),
                recursive: true,
            }],
            roots: vec![DestinationRoot {
                name: name.clone(),
                target: root.clone(),
            }],
        };
        transfer(
            &HeldSession(&source),
            &destination,
            request,
            TransferOptions::default().with_discovery(discovery),
        )
        .await
        .unwrap();
        assert_eq!(
            destination
                .metadata()
                .snapshot()
                .await
                .unwrap()
                .root(&name)
                .await
                .unwrap(),
            Some(root.clone())
        );
        assert!(matches!(
            destination.verify_closure(&root).await.unwrap(),
            ClosureStatus::Complete { .. }
        ));
    }
}

struct WrongPayloadSession;

#[async_trait]
impl TransferReadSession for WrongPayloadSession {
    fn revision(&self) -> RepositoryRevision {
        unreachable!()
    }
    async fn object(&self, _: &ObjectKey) -> Result<Option<ObjectRecord>, TransferError> {
        unreachable!()
    }
    async fn root(&self, _: &RootName) -> Result<Option<ObjectKey>, TransferError> {
        unreachable!()
    }
    async fn open_payload(
        &self,
        _: &ObjectRecord,
    ) -> Result<Option<TransferPayloadReader>, TransferError> {
        Ok(Some(Box::new(std::io::Cursor::new(b"incorrect bytes"))))
    }
}

#[tokio::test]
async fn split_source_missing_or_wrong_blobs_do_not_fallback_or_move_roots() {
    let (metadata, name, root, _, _, _) = filesystem_source().await;
    let empty = repository();
    for corrupt in [false, true] {
        let destination = repository();
        let mutation = destination.mutation_session().await.unwrap();
        let old = mutation.stage_blob(b"old root").await.unwrap();
        let old_key = old.record().key().clone();
        mutation
            .publish(
                vec![old],
                vec![RootChange::Set {
                    name: name.clone(),
                    target: old_key.clone(),
                }],
            )
            .await
            .unwrap();
        drop(mutation);
        let blobs: Box<dyn TransferReadSession> = if corrupt {
            Box::new(WrongPayloadSession)
        } else {
            empty
                .begin_transfer(crate::sync::TransferSelection::Snapshot)
                .await
                .unwrap()
        };
        let source = SplitTransferSession::new(
            metadata
                .begin_transfer(crate::sync::TransferSelection::Snapshot)
                .await
                .unwrap(),
            blobs,
        );
        let error = transfer(
            &HeldSession(&source),
            &destination,
            TransferRequest {
                objects: vec![ObjectRequest {
                    key: root.clone(),
                    recursive: true,
                }],
                roots: vec![DestinationRoot {
                    name: name.clone(),
                    target: root.clone(),
                }],
            },
            TransferOptions::default(),
        )
        .await
        .unwrap_err();
        if corrupt {
            assert!(matches!(
                error,
                TransferError::Destination(RepositoryError::PayloadIdentityMismatch { .. })
            ));
        } else {
            assert!(matches!(error, TransferError::IncompleteSource { .. }));
        }
        assert_eq!(
            destination
                .metadata()
                .snapshot()
                .await
                .unwrap()
                .root(&name)
                .await
                .unwrap(),
            Some(old_key)
        );
    }
}

struct DiscoveryProbe<'a> {
    inner: Box<dyn TransferReadSession + 'a>,
    lookups: AtomicUsize,
    hidden: Option<ObjectKey>,
}

#[async_trait]
impl TransferReadSession for DiscoveryProbe<'_> {
    fn revision(&self) -> RepositoryRevision {
        self.inner.revision()
    }

    async fn object(&self, key: &ObjectKey) -> Result<Option<ObjectRecord>, TransferError> {
        self.lookups.fetch_add(1, Ordering::Relaxed);
        if self.hidden.as_ref() == Some(key) {
            return Ok(None);
        }
        self.inner.object(key).await
    }

    async fn root(&self, name: &RootName) -> Result<Option<ObjectKey>, TransferError> {
        self.inner.root(name).await
    }

    async fn open_payload(
        &self,
        record: &ObjectRecord,
    ) -> Result<Option<TransferPayloadReader>, TransferError> {
        self.inner.open_payload(record).await
    }
}

#[tokio::test]
async fn caller_owned_transfer_retains_reused_closures() {
    let (source, _, root, _, leaf, _) = filesystem_source().await;
    let source = source
        .begin_transfer(crate::sync::TransferSelection::Snapshot)
        .await
        .unwrap();
    let request = TransferRequest {
        objects: vec![ObjectRequest {
            key: root.clone(),
            recursive: true,
        }],
        roots: vec![],
    };
    for discovery in [
        TransferDiscovery::Exhaustive,
        TransferDiscovery::ReuseVerified,
    ] {
        let destination = repository();
        let first = destination.mutation_session().await.unwrap();
        transfer_into_mutation(source.as_ref(), &first, request.clone(), None, discovery)
            .await
            .unwrap();
        let second = destination.mutation_session().await.unwrap();
        let result =
            transfer_into_mutation(source.as_ref(), &second, request.clone(), None, discovery)
                .await
                .unwrap();
        assert_eq!(result.progress.published_objects, 0);
        drop(first);

        destination.collect().await.unwrap();
        assert!(matches!(
            destination.verify_closure(&root).await.unwrap(),
            ClosureStatus::Complete { .. }
        ));
        assert!(matches!(
            destination.verify_closure(&leaf).await.unwrap(),
            ClosureStatus::Complete { .. }
        ));
        drop(second);
        destination.collect().await.unwrap();
        for key in [&root, &leaf] {
            assert!(matches!(
                destination.verify_closure(key).await.unwrap(),
                ClosureStatus::Missing { .. }
            ));
        }
    }
}

#[tokio::test]
async fn incremental_discovery_reuses_only_complete_destination_closures() {
    let (source, name, root, sub, leaf, _) = filesystem_source().await;
    let request = TransferRequest {
        objects: vec![ObjectRequest {
            key: root.clone(),
            recursive: true,
        }],
        roots: vec![DestinationRoot {
            name,
            target: root.clone(),
        }],
    };
    for memory_objects in [2, 250_000] {
        let destination = repository().with_spill_limits(crate::SpillLimits {
            max_memory_objects: memory_objects,
            max_spill_bytes: 64 * 1024 * 1024,
        });
        // A shallow record cannot stand in for a complete local closure.
        transfer(
            &source,
            &destination,
            TransferRequest {
                objects: vec![ObjectRequest {
                    key: sub.clone(),
                    recursive: false,
                }],
                roots: Vec::new(),
            },
            TransferOptions::default(),
        )
        .await
        .unwrap();
        let session = DiscoveryProbe {
            inner: source
                .begin_transfer(crate::sync::TransferSelection::Snapshot)
                .await
                .unwrap(),
            lookups: AtomicUsize::new(0),
            hidden: None,
        };
        transfer(
            &HeldSession(&session),
            &destination,
            request.clone(),
            TransferOptions::default().with_discovery(TransferDiscovery::ReuseVerified),
        )
        .await
        .unwrap();
        assert_eq!(session.lookups.load(Ordering::Relaxed), 5);
        assert!(
            destination
                .metadata()
                .snapshot()
                .await
                .unwrap()
                .object(&leaf)
                .await
                .unwrap()
                .is_some()
        );

        // Reuse checks the root record but deliberately does not audit a
        // source descendant. Exhaustive mode must still report its absence.
        let incomplete = DiscoveryProbe {
            inner: source
                .begin_transfer(crate::sync::TransferSelection::Snapshot)
                .await
                .unwrap(),
            lookups: AtomicUsize::new(0),
            hidden: Some(leaf.clone()),
        };
        let result = transfer(
            &HeldSession(&incomplete),
            &destination,
            request.clone(),
            TransferOptions::default().with_discovery(TransferDiscovery::ReuseVerified),
        )
        .await
        .unwrap();
        assert_eq!(result.progress.published_objects, 0);
        assert_eq!(incomplete.lookups.load(Ordering::Relaxed), 1);
        assert!(matches!(
            transfer(
                &HeldSession(&incomplete),
                &destination,
                request.clone(),
                TransferOptions::default()
            )
            .await,
            Err(TransferError::IncompleteSource { .. })
        ));
    }
}

#[tokio::test]
async fn spilled_discovery_upgrades_an_already_selected_shallow_object() {
    let (source, _, root, sub, leaf, _) = filesystem_source().await;
    for memory_objects in [2, 250_000] {
        let destination = Repository::with_formats(
            MemoryBlobStore::new(),
            MemoryMetadataStore::new().unwrap(),
            FormatRegistry::builtin(),
            FormatLimits {
                // Upgrading the shallow selection must not count it twice.
                max_traversal_objects: 5,
                ..FormatLimits::default()
            },
        )
        .with_spill_limits(crate::SpillLimits {
            max_memory_objects: memory_objects,
            max_spill_bytes: 64 * 1024 * 1024,
        });
        let result = transfer(
            &source,
            &destination,
            TransferRequest {
                objects: vec![
                    ObjectRequest {
                        key: sub.clone(),
                        recursive: false,
                    },
                    ObjectRequest {
                        key: root.clone(),
                        recursive: true,
                    },
                ],
                roots: Vec::new(),
            },
            TransferOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(result.progress.published_objects, 5);
        assert!(
            destination
                .metadata()
                .snapshot()
                .await
                .unwrap()
                .object(&leaf)
                .await
                .unwrap()
                .is_some()
        );
    }
    let destination = repository().with_spill_limits(crate::SpillLimits {
        max_memory_objects: 1,
        max_spill_bytes: 0,
    });
    assert!(
        transfer(
            &source,
            &destination,
            TransferRequest {
                objects: vec![ObjectRequest {
                    key: root,
                    recursive: true
                }],
                roots: Vec::new(),
            },
            TransferOptions::default()
        )
        .await
        .is_err()
    );
}

async fn filesystem_source() -> (
    Repository<MemoryBlobStore, MemoryMetadataStore>,
    RootName,
    ObjectKey,
    ObjectKey,
    ObjectKey,
    ObjectKey,
) {
    let source = repository();
    let name = RootName::try_from("releases/current").unwrap();
    let mutation = source.mutation_session().await.unwrap();
    let selected = mutation.stage_blob(b"selected").await.unwrap();
    let selected_key = selected.record().key().clone();
    let sibling = mutation.stage_blob(b"sibling").await.unwrap();
    let sibling_key = sibling.record().key().clone();
    let outside = mutation.stage_blob(b"outside").await.unwrap();

    let sub = Directory::try_from_iter([
        (
            pc("selected.txt"),
            Node::File {
                digest: BlobId::new(Digest::hash(b"selected")),
                size: b"selected".len() as u64,
                executable: false,
            },
        ),
        (
            pc("sibling.txt"),
            Node::File {
                digest: BlobId::new(Digest::hash(b"sibling")),
                size: b"sibling".len() as u64,
                executable: false,
            },
        ),
    ])
    .unwrap();
    let sub_object = mutation.stage_directory(&sub).await.unwrap();
    let sub_key = sub_object.record().key().clone();
    let root = Directory::try_from_iter([
        (
            pc("sub"),
            Node::Directory {
                digest: sub.digest(),
                size: sub.size(),
            },
        ),
        (
            pc("outside.txt"),
            Node::File {
                digest: BlobId::new(Digest::hash(b"outside")),
                size: b"outside".len() as u64,
                executable: false,
            },
        ),
    ])
    .unwrap();
    let root_object = mutation.stage_directory(&root).await.unwrap();
    let root_key = root_object.record().key().clone();
    mutation
        .publish(
            vec![selected, sibling, outside, sub_object, root_object],
            vec![RootChange::Set {
                name: name.clone(),
                target: root_key.clone(),
            }],
        )
        .await
        .unwrap();
    drop(mutation);
    (source, name, root_key, sub_key, selected_key, sibling_key)
}

async fn proof_entry(session: &dyn TransferReadSession, key: &ObjectKey) -> PathProofEntry {
    let record = session.object(key).await.unwrap().unwrap();
    let mut reader = session.open_payload(&record).await.unwrap().unwrap();
    let mut payload = Vec::new();
    reader.read_to_end(&mut payload).await.unwrap();
    PathProofEntry { record, payload }
}

struct RootOverrideSession<'a> {
    inner: Box<dyn TransferReadSession + 'a>,
    root: ObjectKey,
}

#[async_trait]
impl TransferReadSession for RootOverrideSession<'_> {
    fn revision(&self) -> RepositoryRevision {
        self.inner.revision()
    }

    async fn object(&self, key: &ObjectKey) -> Result<Option<ObjectRecord>, TransferError> {
        self.inner.object(key).await
    }

    async fn root(&self, _name: &RootName) -> Result<Option<ObjectKey>, TransferError> {
        Ok(Some(self.root.clone()))
    }

    async fn open_payload(
        &self,
        record: &ObjectRecord,
    ) -> Result<Option<TransferPayloadReader>, TransferError> {
        self.inner.open_payload(record).await
    }

    async fn chunks(&self, record: &ObjectRecord) -> Result<Option<Vec<ChunkMeta>>, TransferError> {
        self.inner.chunks(record).await
    }

    fn as_chunk_source(&self) -> Option<&dyn BlobChunkSource> {
        self.inner.as_chunk_source()
    }
}

struct PayloadConcurrencyProbe {
    active: AtomicUsize,
    peak: AtomicUsize,
    expected: usize,
    reached: Notify,
    release: Semaphore,
}

struct ActivePayloadOpen<'a>(&'a AtomicUsize);

impl Drop for ActivePayloadOpen<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

struct ProbedTransferSource {
    repository: Repository<MemoryBlobStore, MemoryMetadataStore>,
    probe: Arc<PayloadConcurrencyProbe>,
}

struct ProbedTransferSession<'a> {
    inner: Box<dyn TransferReadSession + 'a>,
    probe: Arc<PayloadConcurrencyProbe>,
}

#[async_trait]
impl TransferReadSession for ProbedTransferSession<'_> {
    fn revision(&self) -> RepositoryRevision {
        self.inner.revision()
    }

    async fn object(&self, key: &ObjectKey) -> Result<Option<ObjectRecord>, TransferError> {
        self.inner.object(key).await
    }

    async fn root(&self, name: &RootName) -> Result<Option<ObjectKey>, TransferError> {
        self.inner.root(name).await
    }

    async fn open_payload(
        &self,
        record: &ObjectRecord,
    ) -> Result<Option<TransferPayloadReader>, TransferError> {
        let active = self.probe.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.probe.peak.fetch_max(active, Ordering::SeqCst);
        let _active = ActivePayloadOpen(&self.probe.active);
        if active == self.probe.expected {
            self.probe.reached.notify_one();
        }
        self.probe
            .release
            .acquire()
            .await
            .expect("payload probe semaphore remains open")
            .forget();
        self.inner.open_payload(record).await
    }
}

#[async_trait]
impl TransferSource for ProbedTransferSource {
    async fn begin_transfer(
        &self,
        selection: TransferSelection,
    ) -> Result<Box<dyn TransferReadSession + '_>, TransferError> {
        Ok(Box::new(ProbedTransferSession {
            inner: self.repository.begin_transfer(selection).await?,
            probe: self.probe.clone(),
        }))
    }
}

#[derive(Clone)]
struct OneShotFailingMetadataStore {
    inner: MemoryMetadataStore,
    commit_attempts: Arc<AtomicUsize>,
    fail_at: Arc<AtomicUsize>,
}

#[async_trait]
impl MetadataStore for OneShotFailingMetadataStore {
    async fn try_collection_lease(
        &self,
    ) -> Result<Option<crate::metadata::RepositoryLease>, crate::metadata::MetadataError> {
        self.inner.try_collection_lease().await
    }
    fn coordinates_payload_catalog(&self) -> bool {
        self.inner.coordinates_payload_catalog()
    }
    async fn pin_store(
        &self,
    ) -> Result<std::sync::Arc<dyn crate::metadata::PinStore>, crate::metadata::MetadataError> {
        self.inner.pin_store().await
    }

    async fn snapshot(&self) -> Result<Arc<dyn MetadataSnapshot>, MetadataError> {
        self.inner.snapshot().await
    }

    async fn commit(
        &self,
        expected: &RepositoryRevision,
        mutation: MetadataMutation,
    ) -> Result<CommitResult, MetadataError> {
        let attempt = self.commit_attempts.fetch_add(1, Ordering::SeqCst) + 1;
        if self
            .fail_at
            .compare_exchange(attempt, usize::MAX, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return Err(MetadataError::Backend(format!(
                "injected failure at commit {attempt}"
            )));
        }
        self.inner.commit(expected, mutation).await
    }
}

struct BlockingSnapshot {
    inner: Arc<dyn MetadataSnapshot>,
    blocked: AtomicBool,
    reached: Arc<Notify>,
    resume: Arc<Notify>,
}

#[async_trait]
impl MetadataSnapshot for BlockingSnapshot {
    fn generation(&self) -> Result<u64, MetadataError> {
        self.inner.generation()
    }
    fn objects_created_through(
        &self,
        generation: u64,
    ) -> futures::stream::BoxStream<'static, Result<ObjectRecord, MetadataError>> {
        self.inner.objects_created_through(generation)
    }

    fn revision(&self) -> RepositoryRevision {
        self.inner.revision()
    }

    fn retention_resources(&self) -> std::collections::BTreeSet<crate::metadata::PinResource> {
        self.inner.retention_resources()
    }

    async fn object(&self, key: &ObjectKey) -> Result<Option<ObjectRecord>, MetadataError> {
        if !self.blocked.swap(true, Ordering::SeqCst) {
            self.reached.notify_one();
            self.resume.notified().await;
        }
        self.inner.object(key).await
    }

    async fn root(&self, name: &RootName) -> Result<Option<ObjectKey>, MetadataError> {
        self.inner.root(name).await
    }

    fn objects(&self) -> BoxStream<'static, Result<ObjectRecord, MetadataError>> {
        self.inner.objects()
    }

    fn roots(&self) -> BoxStream<'static, Result<RootRecord, MetadataError>> {
        self.inner.roots()
    }
}

struct BlockingTransferSource {
    repository: Repository<MemoryBlobStore, MemoryMetadataStore>,
    reached: Arc<Notify>,
    resume: Arc<Notify>,
}

struct BlockingTransferSession<'a> {
    repository: &'a Repository<MemoryBlobStore, MemoryMetadataStore>,
    _hold: crate::RetentionHold<'a, MemoryBlobStore, MemoryMetadataStore>,
    snapshot: BlockingSnapshot,
}

#[async_trait]
impl TransferReadSession for BlockingTransferSession<'_> {
    fn revision(&self) -> RepositoryRevision {
        self.snapshot.revision()
    }

    async fn object(&self, key: &ObjectKey) -> Result<Option<ObjectRecord>, TransferError> {
        self.snapshot
            .object(key)
            .await
            .map_err(TransferError::SourceMetadata)
    }

    async fn root(&self, name: &RootName) -> Result<Option<ObjectKey>, TransferError> {
        self.snapshot
            .root(name)
            .await
            .map_err(TransferError::SourceMetadata)
    }

    async fn open_payload(
        &self,
        record: &ObjectRecord,
    ) -> Result<Option<TransferPayloadReader>, TransferError> {
        self.repository
            .payloads()
            .open_read(&record.payload())
            .await
            .map(|reader| reader.map(|reader| reader as TransferPayloadReader))
            .map_err(|error| TransferError::SourcePayload {
                key: record.key().clone(),
                error,
            })
    }

    async fn chunks(&self, record: &ObjectRecord) -> Result<Option<Vec<ChunkMeta>>, TransferError> {
        self.repository
            .payloads()
            .chunks(&record.payload())
            .await
            .map_err(|error| TransferError::SourcePayload {
                key: record.key().clone(),
                error,
            })
    }

    fn as_chunk_source(&self) -> Option<&dyn BlobChunkSource> {
        self.repository.payloads().as_chunk_source()
    }
}

#[async_trait]
impl TransferSource for BlockingTransferSource {
    async fn begin_transfer(
        &self,
        selection: TransferSelection,
    ) -> Result<Box<dyn TransferReadSession + '_>, TransferError> {
        let hold = self.repository.transfer_hold(&selection).await?;
        let snapshot = self
            .repository
            .metadata()
            .snapshot()
            .await
            .map_err(TransferError::SourceMetadata)?;
        Ok(Box::new(BlockingTransferSession {
            repository: &self.repository,
            _hold: hold,
            snapshot: BlockingSnapshot {
                inner: snapshot,
                blocked: AtomicBool::new(false),
                reached: self.reached.clone(),
                resume: self.resume.clone(),
            },
        }))
    }
}

#[derive(Clone)]
struct OneShotFailingReadStore {
    inner: MemoryBlobStore,
    opens: Arc<AtomicUsize>,
    fail_at: Arc<AtomicUsize>,
}

#[async_trait]
impl BlobStore for OneShotFailingReadStore {
    fn publication(&self) -> crate::blob::PayloadPublication<'_> {
        self.inner.publication()
    }
    fn write_scope(&self) -> crate::metadata::BackendWriteScope {
        self.inner.write_scope()
    }

    fn begin_pinned_batch(
        &self,
        pin: crate::metadata::DataPinLease,
    ) -> Result<crate::blob::BlobBatchGuard, crate::error::Error> {
        self.inner.begin_pinned_batch(pin)
    }

    async fn has(&self, digest: &BlobId) -> Result<bool, crate::error::Error> {
        self.inner.has(digest).await
    }

    async fn open_read(
        &self,
        digest: &BlobId,
    ) -> Result<Option<Box<dyn BlobReader>>, crate::error::Error> {
        let attempt = self.opens.fetch_add(1, Ordering::SeqCst) + 1;
        if self
            .fail_at
            .compare_exchange(attempt, usize::MAX, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return Err(crate::error::Error::Msg(format!(
                "injected payload read failure at open {attempt}"
            )));
        }
        self.inner.open_read(digest).await
    }

    async fn open_write(&self) -> Box<dyn BlobWriter> {
        self.inner.open_write().await
    }

    async fn chunks(&self, digest: &BlobId) -> Result<Option<Vec<ChunkMeta>>, crate::error::Error> {
        self.inner.chunks(digest).await
    }
}

struct FailingReadTransferSource {
    repository: Repository<MemoryBlobStore, MemoryMetadataStore>,
    payloads: OneShotFailingReadStore,
}

struct FailingReadTransferSession<'a> {
    _hold: crate::RetentionHold<'a, MemoryBlobStore, MemoryMetadataStore>,
    snapshot: Arc<dyn MetadataSnapshot>,
    payloads: OneShotFailingReadStore,
}

#[async_trait]
impl TransferReadSession for FailingReadTransferSession<'_> {
    fn revision(&self) -> RepositoryRevision {
        self.snapshot.revision()
    }

    async fn object(&self, key: &ObjectKey) -> Result<Option<ObjectRecord>, TransferError> {
        self.snapshot
            .object(key)
            .await
            .map_err(TransferError::SourceMetadata)
    }

    async fn root(&self, name: &RootName) -> Result<Option<ObjectKey>, TransferError> {
        self.snapshot
            .root(name)
            .await
            .map_err(TransferError::SourceMetadata)
    }

    async fn open_payload(
        &self,
        record: &ObjectRecord,
    ) -> Result<Option<TransferPayloadReader>, TransferError> {
        self.payloads
            .open_read(&record.payload())
            .await
            .map(|reader| reader.map(|reader| reader as TransferPayloadReader))
            .map_err(|error| TransferError::SourcePayload {
                key: record.key().clone(),
                error,
            })
    }

    async fn chunks(&self, record: &ObjectRecord) -> Result<Option<Vec<ChunkMeta>>, TransferError> {
        self.payloads
            .chunks(&record.payload())
            .await
            .map_err(|error| TransferError::SourcePayload {
                key: record.key().clone(),
                error,
            })
    }

    fn as_chunk_source(&self) -> Option<&dyn BlobChunkSource> {
        self.payloads.as_chunk_source()
    }
}

#[async_trait]
impl TransferSource for FailingReadTransferSource {
    async fn begin_transfer(
        &self,
        selection: TransferSelection,
    ) -> Result<Box<dyn TransferReadSession + '_>, TransferError> {
        let hold = self.repository.transfer_hold(&selection).await?;
        let snapshot = self
            .repository
            .metadata()
            .snapshot()
            .await
            .map_err(TransferError::SourceMetadata)?;
        Ok(Box::new(FailingReadTransferSession {
            _hold: hold,
            snapshot,
            payloads: self.payloads.clone(),
        }))
    }
}

#[tokio::test]
async fn recursive_transfer_reverifies_and_roots_an_ipld_graph() {
    let source = repository();
    let mutation = source.mutation_session().await.unwrap();

    // Content-addressed payloads cannot directly construct a literal
    // cycle, so use a shared descendant graph here; cycle traversal is
    // covered by the generic synthetic-format repository tests.
    let leaf_payload = b"leaf";
    let leaf_cid = IpldCid::new(RAW_CODEC, leaf_payload).unwrap();
    let leaf_blob = mutation
        .repository()
        .payloads()
        .put_slice(leaf_payload)
        .await
        .unwrap();
    let leaf = mutation
        .stage_existing(leaf_cid.object_key().unwrap(), leaf_blob)
        .await
        .unwrap();
    let linked_payload = LinkedIpld::encode(std::slice::from_ref(&leaf_cid), b"root").unwrap();
    let linked_cid = IpldCid::new(crate::CASITA_LINKED_CODEC, &linked_payload).unwrap();
    let linked_blob = mutation
        .repository()
        .payloads()
        .put_slice(&linked_payload)
        .await
        .unwrap();
    let linked = mutation
        .stage_existing(linked_cid.object_key().unwrap(), linked_blob)
        .await
        .unwrap();
    let root_key = linked.record().key().clone();
    mutation.publish_unrooted(vec![leaf, linked]).await.unwrap();
    drop(mutation);

    let destination = repository();
    let request = TransferRequest {
        objects: vec![ObjectRequest {
            key: root_key.clone(),
            recursive: true,
        }],
        roots: vec![DestinationRoot {
            name: "ipld/main".parse().unwrap(),
            target: root_key.clone(),
        }],
    };
    let first = transfer(
        &source,
        &destination,
        request.clone(),
        TransferOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(first.progress.published_objects, 2);
    assert_eq!(first.progress.payloads_sent, 2);
    assert_eq!(
        first.progress.requested,
        vec![RequestedStatus::Complete(root_key.clone())]
    );

    let second = transfer(&source, &destination, request, TransferOptions::default())
        .await
        .unwrap();
    assert_eq!(second.progress.published_objects, 0);
    assert_eq!(second.progress.payloads_sent, 0);
    assert_eq!(second.progress.payloads_reused, 0);
    let snapshot = destination.metadata().snapshot().await.unwrap();
    assert_eq!(
        snapshot
            .root(&RootName::try_from("ipld/main").unwrap())
            .await
            .unwrap(),
        Some(root_key)
    );
}

#[tokio::test]
async fn path_transfer_keeps_spine_and_siblings_out_of_destination() {
    let (source, source_name, root, sub, selected, sibling) = filesystem_source().await;
    let destination = repository();
    let destination_name = RootName::try_from("partial/selected").unwrap();
    let outcome = transfer_path(
        &source,
        &destination,
        &source_name,
        "sub/selected.txt",
        Some(destination_name.clone()),
        TransferOptions::default(),
    )
    .await
    .unwrap();
    assert!(matches!(outcome.node, Some(Node::File { .. })));
    let progress = outcome.transfer.unwrap().progress;
    assert_eq!(progress.published_objects, 1);
    assert_eq!(
        progress.requested,
        vec![RequestedStatus::Complete(selected.clone())]
    );

    let snapshot = destination.metadata().snapshot().await.unwrap();
    assert_eq!(
        snapshot.root(&destination_name).await.unwrap(),
        Some(selected.clone())
    );
    assert!(snapshot.object(&selected).await.unwrap().is_some());
    assert!(snapshot.object(&root).await.unwrap().is_none());
    assert!(snapshot.object(&sub).await.unwrap().is_none());
    assert!(snapshot.object(&sibling).await.unwrap().is_none());
}

#[tokio::test]
async fn path_transfer_copies_only_the_selected_subtree_closure() {
    let (source, source_name, root, sub, selected, sibling) = filesystem_source().await;
    let destination = repository();
    let outcome = transfer_path(
        &source,
        &destination,
        &source_name,
        "/sub/",
        None,
        TransferOptions::default(),
    )
    .await
    .unwrap();
    assert!(matches!(
        outcome.node,
        Some(Node::Directory { digest, size: 2 }) if ObjectKey::directory(digest) == sub
    ));
    assert_eq!(outcome.transfer.unwrap().progress.published_objects, 3);

    let snapshot = destination.metadata().snapshot().await.unwrap();
    assert!(snapshot.object(&sub).await.unwrap().is_some());
    assert!(snapshot.object(&selected).await.unwrap().is_some());
    assert!(snapshot.object(&sibling).await.unwrap().is_some());
    assert!(snapshot.object(&root).await.unwrap().is_none());
    assert!(
        snapshot
            .object(&ObjectKey::blob(BlobId::new(Digest::hash(b"outside"))))
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn path_transfer_authenticates_absence_and_rejects_traversal() {
    let (source, source_name, root, sub, selected, _) = filesystem_source().await;
    let destination = repository();
    let absent = transfer_path(
        &source,
        &destination,
        &source_name,
        "sub/missing.txt",
        None,
        TransferOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        absent,
        PathTransferResult {
            node: None,
            transfer: None
        }
    );
    let snapshot = destination.metadata().snapshot().await.unwrap();
    assert!(snapshot.object(&root).await.unwrap().is_none());
    assert!(snapshot.object(&sub).await.unwrap().is_none());
    assert!(snapshot.object(&selected).await.unwrap().is_none());

    assert!(matches!(
        transfer_path(
            &source,
            &destination,
            &source_name,
            "../secret",
            None,
            TransferOptions::default()
        )
        .await,
        Err(TransferError::InvalidRequest(_))
    ));
}

#[tokio::test]
async fn path_transfer_rejects_a_false_parent_size_before_rooting() {
    let source = repository();
    let mutation = source.mutation_session().await.unwrap();
    let selected = mutation.stage_blob(b"selected").await.unwrap();
    let selected_key = selected.record().key().clone();
    let dishonest_root = Directory::try_from_iter([(
        pc("selected.txt"),
        Node::File {
            digest: BlobId::new(Digest::hash(b"selected")),
            size: 999,
            executable: false,
        },
    )])
    .unwrap();
    let root_object = mutation.stage_directory(&dishonest_root).await.unwrap();
    let root_key = root_object.record().key().clone();
    mutation
        .publish_unrooted(vec![selected, root_object])
        .await
        .unwrap();
    drop(mutation);

    let source_session = source
        .begin_transfer(crate::sync::TransferSelection::Snapshot)
        .await
        .unwrap();
    let source_session = RootOverrideSession {
        inner: source_session,
        root: root_key,
    };
    let destination = repository();
    let destination_name = RootName::try_from("partial/selected").unwrap();
    let error = transfer_path(
        &HeldSession(&source_session),
        &destination,
        &RootName::try_from("untrusted/root").unwrap(),
        "selected.txt",
        Some(destination_name.clone()),
        TransferOptions::default(),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        TransferError::InvalidSourceSelection { key, .. } if key == selected_key
    ));
    assert!(
        destination
            .metadata()
            .snapshot()
            .await
            .unwrap()
            .root(&destination_name)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn path_proof_verification_rejects_every_untrusted_boundary() {
    let (source, source_name, root, sub, selected, _) = filesystem_source().await;
    let session = source
        .begin_transfer(crate::sync::TransferSelection::Snapshot)
        .await
        .unwrap();
    let revision = session.revision();
    let root_entry = proof_entry(session.as_ref(), &root).await;
    let sub_entry = proof_entry(session.as_ref(), &sub).await;
    let selected_entry = proof_entry(session.as_ref(), &selected).await;
    let destination = repository();
    let components = vec![pc("sub"), pc("selected.txt")];

    let resolved = resolve_verified_path_proof(
        session.as_ref(),
        &destination,
        &source_name,
        &components,
        PathProof {
            revision,
            root_name: source_name.clone(),
            root: root.clone(),
            directories: vec![root_entry.clone(), sub_entry.clone()],
        },
    )
    .await
    .unwrap();
    assert!(matches!(resolved, Some(Node::File { .. })));

    let wrong_revision = resolve_verified_path_proof(
        session.as_ref(),
        &destination,
        &source_name,
        &components,
        PathProof {
            revision: RepositoryRevision::from_bytes([0xa5; 32]),
            root_name: source_name.clone(),
            root: root.clone(),
            directories: vec![root_entry.clone(), sub_entry.clone()],
        },
    )
    .await;
    assert!(matches!(
        wrong_revision,
        Err(TransferError::SourceTransport(_))
    ));

    let wrong_root_name = resolve_verified_path_proof(
        session.as_ref(),
        &destination,
        &source_name,
        &components,
        PathProof {
            revision,
            root_name: RootName::try_from("releases/other").unwrap(),
            root: root.clone(),
            directories: vec![root_entry.clone(), sub_entry.clone()],
        },
    )
    .await;
    assert!(matches!(
        wrong_root_name,
        Err(TransferError::SourceTransport(_))
    ));

    let wrong_root = resolve_verified_path_proof(
        session.as_ref(),
        &destination,
        &source_name,
        &components,
        PathProof {
            revision,
            root_name: source_name.clone(),
            root: sub.clone(),
            directories: vec![root_entry.clone(), sub_entry.clone()],
        },
    )
    .await;
    assert!(matches!(wrong_root, Err(TransferError::SourceTransport(_))));

    let missing_record = resolve_verified_path_proof(
        session.as_ref(),
        &destination,
        &source_name,
        &components,
        PathProof {
            revision,
            root_name: source_name.clone(),
            root: root.clone(),
            directories: vec![root_entry.clone()],
        },
    )
    .await;
    assert!(matches!(
        missing_record,
        Err(TransferError::InvalidSourceSelection { .. })
    ));

    let reordered = resolve_verified_path_proof(
        session.as_ref(),
        &destination,
        &source_name,
        &components,
        PathProof {
            revision,
            root_name: source_name.clone(),
            root: root.clone(),
            directories: vec![sub_entry.clone(), root_entry.clone()],
        },
    )
    .await;
    assert!(matches!(reordered, Err(TransferError::SourceTransport(_))));

    let wrong_kind = resolve_verified_path_proof(
        session.as_ref(),
        &destination,
        &source_name,
        &components,
        PathProof {
            revision,
            root_name: source_name.clone(),
            root: root.clone(),
            directories: vec![root_entry.clone(), selected_entry],
        },
    )
    .await;
    assert!(matches!(wrong_kind, Err(TransferError::SourceTransport(_))));

    let mut corrupt_entry = root_entry.clone();
    corrupt_entry.payload[0] ^= 0x80;
    let digest_mismatch = resolve_verified_path_proof(
        session.as_ref(),
        &destination,
        &source_name,
        &components,
        PathProof {
            revision,
            root_name: source_name.clone(),
            root: root.clone(),
            directories: vec![corrupt_entry, sub_entry.clone()],
        },
    )
    .await;
    assert!(matches!(
        digest_mismatch,
        Err(TransferError::SourceFormat { .. })
    ));

    let trailing = resolve_verified_path_proof(
        session.as_ref(),
        &destination,
        &source_name,
        &[pc("sub")],
        PathProof {
            revision,
            root_name: source_name.clone(),
            root: root.clone(),
            directories: vec![root_entry.clone(), sub_entry.clone()],
        },
    )
    .await;
    assert!(matches!(
        trailing,
        Err(TransferError::InvalidSourceSelection { .. })
    ));

    let after_file = resolve_verified_path_proof(
        session.as_ref(),
        &destination,
        &source_name,
        &[pc("outside.txt"), pc("deeper")],
        PathProof {
            revision,
            root_name: source_name.clone(),
            root,
            directories: vec![root_entry, sub_entry],
        },
    )
    .await;
    assert!(matches!(
        after_file,
        Err(TransferError::InvalidSourceSelection { .. })
    ));
}

#[tokio::test]
async fn missing_source_boundary_never_changes_requested_roots() {
    let source = repository();
    let destination = repository();
    let missing = ObjectKey::blob(BlobId::new(Digest::hash(b"absent")));
    let error = transfer(
        &source,
        &destination,
        TransferRequest {
            objects: vec![ObjectRequest {
                key: missing.clone(),
                recursive: true,
            }],
            roots: vec![DestinationRoot {
                name: "broken".parse().unwrap(),
                target: missing.clone(),
            }],
        },
        TransferOptions::default(),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        TransferError::IncompleteSource { key } if key == missing
    ));
    assert!(
        destination
            .metadata()
            .snapshot()
            .await
            .unwrap()
            .root(&RootName::try_from("broken").unwrap())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn existing_record_skips_its_payload_but_not_descendant_discovery() {
    let source = repository();
    let mutation = source.mutation_session().await.unwrap();
    let leaf_payload = b"later descendant";
    let leaf_cid = IpldCid::new(RAW_CODEC, leaf_payload).unwrap();
    let leaf_blob = mutation
        .repository()
        .payloads()
        .put_slice(leaf_payload)
        .await
        .unwrap();
    let leaf = mutation
        .stage_existing(leaf_cid.object_key().unwrap(), leaf_blob)
        .await
        .unwrap();
    let root_payload = LinkedIpld::encode(std::slice::from_ref(&leaf_cid), b"").unwrap();
    let root_cid = IpldCid::new(crate::CASITA_LINKED_CODEC, &root_payload).unwrap();
    let root_blob = mutation
        .repository()
        .payloads()
        .put_slice(&root_payload)
        .await
        .unwrap();
    let root = mutation
        .stage_existing(root_cid.object_key().unwrap(), root_blob)
        .await
        .unwrap();
    let root_key = root.record().key().clone();
    mutation.publish_unrooted(vec![leaf, root]).await.unwrap();
    drop(mutation);

    let destination = repository();
    let shallow = transfer(
        &source,
        &destination,
        TransferRequest {
            objects: vec![ObjectRequest {
                key: root_key.clone(),
                recursive: false,
            }],
            roots: Vec::new(),
        },
        TransferOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(shallow.progress.published_objects, 1);
    assert_eq!(
        shallow.progress.requested,
        vec![RequestedStatus::Incomplete(root_key.clone())]
    );

    let recursive = transfer(
        &source,
        &destination,
        TransferRequest {
            objects: vec![ObjectRequest {
                key: root_key.clone(),
                recursive: true,
            }],
            roots: Vec::new(),
        },
        TransferOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(recursive.progress.published_objects, 1);
    assert_eq!(recursive.progress.payloads_sent, 1);
    assert_eq!(
        recursive.progress.requested,
        vec![RequestedStatus::Complete(root_key)]
    );
}

fn chunked_repository(average: u32) -> Repository<ChunkedBlobStore, MemoryMetadataStore> {
    let objects: Arc<dyn object_store::ObjectStore> =
        Arc::new(object_store::memory::InMemory::new());
    Repository::new(
        ChunkedBlobStore::new(objects, object_store::path::Path::default(), average),
        MemoryMetadataStore::new().unwrap(),
    )
}

#[tokio::test]
async fn physical_capability_selects_chunks_or_whole_payload_without_changing_result() {
    let content: Vec<_> = (0..700_000usize)
        .map(|index| ((index * 31) % 251) as u8)
        .collect();
    let source = chunked_repository(32 * 1024);
    let mutation = source.mutation_session().await.unwrap();
    let object = mutation.stage_blob(&content).await.unwrap();
    let key = object.record().key().clone();
    mutation.publish_unrooted(vec![object]).await.unwrap();
    drop(mutation);

    let chunked_destination = chunked_repository(128 * 1024);
    let request = TransferRequest {
        objects: vec![ObjectRequest {
            key: key.clone(),
            recursive: true,
        }],
        roots: Vec::new(),
    };
    let optimized = transfer(
        &source,
        &chunked_destination,
        request.clone(),
        TransferOptions::default(),
    )
    .await
    .unwrap();
    assert!(optimized.progress.chunks_sent > 0);
    assert_eq!(optimized.progress.payloads_sent, 1);

    let memory_destination = repository();
    let fallback = transfer(
        &source,
        &memory_destination,
        request,
        TransferOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(fallback.progress.chunks_sent, 0);
    assert_eq!(fallback.progress.payloads_sent, 1);
    assert_eq!(
        memory_destination
            .payloads()
            .read_to_vec(&BlobId::new(Digest::hash(&content)))
            .await
            .unwrap()
            .unwrap(),
        content
    );
}

// This source intentionally has no BlobStore or BlobSync implementation:
// remote chunk access must not require write capabilities.
struct ReadOnlyChunkSession<'a> {
    inner: Box<dyn TransferReadSession + 'a>,
    reads: AtomicUsize,
    corrupt: bool,
}

#[async_trait]
impl BlobChunkSource for ReadOnlyChunkSession<'_> {
    async fn get_chunk(
        &self,
        digest: &crate::ChunkId,
    ) -> Result<Option<bytes::Bytes>, crate::error::Error> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        if self.corrupt {
            return Ok(Some(zstd::encode_all(b"wrong chunk".as_slice(), 0)?.into()));
        }
        self.inner
            .as_chunk_source()
            .unwrap()
            .get_chunk(digest)
            .await
    }
}

#[async_trait]
impl TransferReadSession for ReadOnlyChunkSession<'_> {
    fn revision(&self) -> RepositoryRevision {
        self.inner.revision()
    }
    async fn object(&self, key: &ObjectKey) -> Result<Option<ObjectRecord>, TransferError> {
        self.inner.object(key).await
    }
    async fn root(&self, name: &RootName) -> Result<Option<ObjectKey>, TransferError> {
        self.inner.root(name).await
    }
    async fn open_payload(
        &self,
        record: &ObjectRecord,
    ) -> Result<Option<TransferPayloadReader>, TransferError> {
        self.inner.open_payload(record).await
    }
    async fn chunks(&self, record: &ObjectRecord) -> Result<Option<Vec<ChunkMeta>>, TransferError> {
        self.inner.chunks(record).await
    }
    fn as_chunk_source(&self) -> Option<&dyn BlobChunkSource> {
        Some(self)
    }
}

#[tokio::test]
async fn read_only_chunk_source_reuses_destination_chunks_and_preserves_streaming_fallback() {
    let content: Vec<_> = (0..700_000usize)
        .map(|index| ((index * 31) % 251) as u8)
        .collect();
    let original = chunked_repository(32 * 1024);
    let mutation = original.mutation_session().await.unwrap();
    let staged = mutation.stage_blob(&content).await.unwrap();
    let key = staged.record().key().clone();
    let payload = staged.record().payload();
    mutation.publish_unrooted(vec![staged]).await.unwrap();
    drop(mutation);
    let chunks = original.payloads().chunks(&payload).await.unwrap().unwrap();
    assert!(chunks.len() > 1);
    let destination = chunked_repository(128 * 1024);
    let first = &chunks[0];
    let bytes = original
        .payloads()
        .get_chunk(&first.digest)
        .await
        .unwrap()
        .unwrap();
    destination
        .payloads()
        .as_blob_sync()
        .unwrap()
        .put_chunk(first, bytes)
        .await
        .unwrap();
    let expected_missing = chunks
        .iter()
        .map(|chunk| chunk.digest)
        .filter(|digest| *digest != first.digest)
        .collect::<std::collections::HashSet<_>>()
        .len();
    assert!(expected_missing > 0);
    let source = ReadOnlyChunkSession {
        inner: original
            .begin_transfer(crate::sync::TransferSelection::Snapshot)
            .await
            .unwrap(),
        reads: AtomicUsize::new(0),
        corrupt: false,
    };
    let request = TransferRequest {
        objects: vec![ObjectRequest {
            key: key.clone(),
            recursive: true,
        }],
        roots: vec![DestinationRoot {
            name: "copy".parse().unwrap(),
            target: key,
        }],
    };
    let result = transfer(
        &HeldSession(&source),
        &destination,
        request.clone(),
        TransferOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(result.progress.chunks_sent as usize, expected_missing);
    assert!(result.progress.chunks_reused > 0);
    assert_eq!(source.reads.load(Ordering::Relaxed), expected_missing);
    assert_eq!(
        destination
            .payloads()
            .read_to_vec(&payload)
            .await
            .unwrap()
            .unwrap(),
        content
    );

    let fallback = repository();
    let result = transfer(
        &HeldSession(&source),
        &fallback,
        request,
        TransferOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(result.progress.chunks_sent, 0);
    assert_eq!(source.reads.load(Ordering::Relaxed), expected_missing);
    assert_eq!(
        fallback
            .payloads()
            .read_to_vec(&payload)
            .await
            .unwrap()
            .unwrap(),
        content
    );
}

#[tokio::test]
async fn read_only_chunk_source_corruption_preserves_the_destination_root() {
    let original = chunked_repository(32 * 1024);
    let mutation = original.mutation_session().await.unwrap();
    let staged = mutation
        .stage_blob(b"correct source payload")
        .await
        .unwrap();
    let key = staged.record().key().clone();
    mutation.publish_unrooted(vec![staged]).await.unwrap();
    drop(mutation);
    let destination = chunked_repository(32 * 1024);
    let mutation = destination.mutation_session().await.unwrap();
    let old = mutation.stage_blob(b"retained old root").await.unwrap();
    let old_key = old.record().key().clone();
    let name = RootName::try_from("copy").unwrap();
    mutation
        .publish_rooted(vec![old], name.clone(), old_key.clone())
        .await
        .unwrap();
    drop(mutation);
    let source = ReadOnlyChunkSession {
        inner: original
            .begin_transfer(crate::sync::TransferSelection::Snapshot)
            .await
            .unwrap(),
        reads: AtomicUsize::new(0),
        corrupt: true,
    };
    let result = transfer(
        &HeldSession(&source),
        &destination,
        TransferRequest {
            objects: vec![ObjectRequest {
                key: key.clone(),
                recursive: true,
            }],
            roots: vec![DestinationRoot {
                name: name.clone(),
                target: key,
            }],
        },
        TransferOptions::default(),
    )
    .await;
    assert!(matches!(
        result,
        Err(TransferError::Destination(RepositoryError::Payload(_)))
    ));
    assert_eq!(source.reads.load(Ordering::Relaxed), 1);
    assert_eq!(
        destination
            .metadata()
            .snapshot()
            .await
            .unwrap()
            .root(&name)
            .await
            .unwrap(),
        Some(old_key)
    );
}

#[tokio::test]
async fn independent_payloads_overlap_under_one_global_physical_limit() {
    let source_repository = repository();
    let mutation = source_repository.mutation_session().await.unwrap();
    let mut keys = Vec::new();
    let mut objects = Vec::new();
    for index in 0..(MAX_CONCURRENT_PHYSICAL_TRANSFERS + 4) {
        let object = mutation
            .stage_blob(format!("parallel payload {index}").as_bytes())
            .await
            .unwrap();
        keys.push(object.record().key().clone());
        objects.push(object);
    }
    mutation.publish_unrooted(objects).await.unwrap();
    drop(mutation);

    let probe = Arc::new(PayloadConcurrencyProbe {
        active: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
        expected: MAX_CONCURRENT_PHYSICAL_TRANSFERS,
        reached: Notify::new(),
        release: Semaphore::new(0),
    });
    let source = ProbedTransferSource {
        repository: source_repository,
        probe: probe.clone(),
    };
    let destination = repository();
    let destination_for_transfer = destination.clone();
    let requested = keys.len();
    let transfer = tokio::spawn(async move {
        transfer(
            &source,
            &destination_for_transfer,
            TransferRequest {
                objects: keys
                    .into_iter()
                    .map(|key| ObjectRequest {
                        key,
                        recursive: false,
                    })
                    .collect(),
                roots: Vec::new(),
            },
            TransferOptions::default(),
        )
        .await
    });

    tokio::time::timeout(std::time::Duration::from_secs(2), probe.reached.notified())
        .await
        .expect("payload transfers did not overlap");
    assert_eq!(
        probe.active.load(Ordering::SeqCst),
        MAX_CONCURRENT_PHYSICAL_TRANSFERS
    );
    assert_eq!(
        probe.peak.load(Ordering::SeqCst),
        MAX_CONCURRENT_PHYSICAL_TRANSFERS
    );
    probe.release.add_permits(requested);

    let result = transfer.await.unwrap().unwrap();
    assert_eq!(result.progress.payloads_sent as usize, requested);
    assert_eq!(result.progress.published_objects as usize, requested);
    assert_eq!(probe.active.load(Ordering::SeqCst), 0);
    assert_eq!(
        probe.peak.load(Ordering::SeqCst),
        MAX_CONCURRENT_PHYSICAL_TRANSFERS
    );
}

#[tokio::test]
async fn zero_mutation_batch_limit_rejects_transfer_before_publication() {
    let source = repository();
    let mutation = source.mutation_session().await.unwrap();
    let object = mutation.stage_blob(b"bounded").await.unwrap();
    let key = object.record().key().clone();
    mutation.publish_unrooted(vec![object]).await.unwrap();
    drop(mutation);

    let destination = Repository::with_formats(
        MemoryBlobStore::new(),
        MemoryMetadataStore::new().unwrap(),
        FormatRegistry::builtin(),
        FormatLimits {
            max_batch_objects: 0,
            ..FormatLimits::default()
        },
    );
    let error = transfer(
        &source,
        &destination,
        TransferRequest {
            objects: vec![ObjectRequest {
                key,
                recursive: false,
            }],
            roots: Vec::new(),
        },
        TransferOptions::default(),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        TransferError::Destination(RepositoryError::LimitExceeded(message))
            if message.contains("nonzero mutation batch limit")
    ));
    assert_eq!(
        destination
            .metadata()
            .snapshot()
            .await
            .unwrap()
            .objects()
            .try_collect::<Vec<_>>()
            .await
            .unwrap(),
        Vec::<ObjectRecord>::new()
    );
}

#[tokio::test]
async fn several_destination_roots_appear_in_one_final_revision() {
    let source = repository();
    let mutation = source.mutation_session().await.unwrap();
    let a = mutation.stage_blob(b"a").await.unwrap();
    let a_key = a.record().key().clone();
    let b = mutation.stage_blob(b"b").await.unwrap();
    let b_key = b.record().key().clone();
    mutation.publish_unrooted(vec![a, b]).await.unwrap();
    drop(mutation);

    let destination = repository();
    let result = transfer(
        &source,
        &destination,
        TransferRequest {
            objects: vec![
                ObjectRequest {
                    key: a_key.clone(),
                    recursive: true,
                },
                ObjectRequest {
                    key: b_key.clone(),
                    recursive: true,
                },
            ],
            roots: vec![
                DestinationRoot {
                    name: "copies/a".parse().unwrap(),
                    target: a_key.clone(),
                },
                DestinationRoot {
                    name: "copies/b".parse().unwrap(),
                    target: b_key.clone(),
                },
            ],
        },
        TransferOptions::default(),
    )
    .await
    .unwrap();
    let snapshot = destination.metadata().snapshot().await.unwrap();
    assert_eq!(snapshot.revision(), result.progress.destination_revision);
    assert_eq!(
        snapshot
            .root(&RootName::try_from("copies/a").unwrap())
            .await
            .unwrap(),
        Some(a_key)
    );
    assert_eq!(
        snapshot
            .root(&RootName::try_from("copies/b").unwrap())
            .await
            .unwrap(),
        Some(b_key)
    );
}

#[tokio::test]
async fn interrupted_bounded_batches_leave_unrooted_progress_and_retry_completes() {
    let source = repository();
    let mutation = source.mutation_session().await.unwrap();
    let mut keys = Vec::new();
    let mut objects = Vec::new();
    for payload in [b"one".as_slice(), b"two".as_slice(), b"three".as_slice()] {
        let object = mutation.stage_blob(payload).await.unwrap();
        keys.push(object.record().key().clone());
        objects.push(object);
    }
    mutation.publish_unrooted(objects).await.unwrap();
    drop(mutation);

    let state = OneShotFailingMetadataStore {
        inner: MemoryMetadataStore::new().unwrap(),
        commit_attempts: Arc::new(AtomicUsize::new(0)),
        fail_at: Arc::new(AtomicUsize::new(2)),
    };
    let destination = Repository::with_formats(
        MemoryBlobStore::new(),
        state.clone(),
        FormatRegistry::builtin(),
        FormatLimits {
            max_batch_objects: 1,
            ..FormatLimits::default()
        },
    );
    let request = TransferRequest {
        objects: keys
            .iter()
            .cloned()
            .map(|key| ObjectRequest {
                key,
                recursive: true,
            })
            .collect(),
        roots: vec![DestinationRoot {
            name: "copies/final".parse().unwrap(),
            target: keys[2].clone(),
        }],
    };

    let error = transfer(
        &source,
        &destination,
        request.clone(),
        TransferOptions::default(),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        TransferError::Destination(RepositoryError::Metadata(MetadataError::Backend(message)))
            if message.contains("injected failure")
    ));
    let interrupted = destination.metadata().snapshot().await.unwrap();
    assert!(interrupted.object(&keys[0]).await.unwrap().is_some());
    assert!(interrupted.object(&keys[1]).await.unwrap().is_none());
    assert!(
        interrupted
            .root(&RootName::try_from("copies/final").unwrap())
            .await
            .unwrap()
            .is_none()
    );
    drop(interrupted);

    let completed = transfer(&source, &destination, request, TransferOptions::default())
        .await
        .unwrap();
    assert_eq!(completed.progress.published_objects, 2);
    assert_eq!(completed.progress.payloads_reused, 1);
    let completed_snapshot = destination.metadata().snapshot().await.unwrap();
    assert_eq!(
        completed_snapshot
            .root(&RootName::try_from("copies/final").unwrap())
            .await
            .unwrap(),
        Some(keys[2].clone())
    );
    assert_eq!(state.commit_attempts.load(Ordering::SeqCst), 5);
}

#[tokio::test]
async fn destination_collection_runs_during_the_receive_operation() {
    let source_repository = repository();
    let mutation = source_repository.mutation_session().await.unwrap();
    let object = mutation.stage_blob(b"held transfer").await.unwrap();
    let key = object.record().key().clone();
    mutation.publish_unrooted(vec![object]).await.unwrap();
    drop(mutation);

    let reached = Arc::new(Notify::new());
    let resume = Arc::new(Notify::new());
    let source = BlockingTransferSource {
        repository: source_repository,
        reached: reached.clone(),
        resume: resume.clone(),
    };
    let destination = repository();
    let destination_for_transfer = destination.clone();
    let transfer = tokio::spawn(async move {
        transfer(
            &source,
            &destination_for_transfer,
            TransferRequest {
                objects: vec![ObjectRequest {
                    key: key.clone(),
                    recursive: true,
                }],
                roots: vec![DestinationRoot {
                    name: "copies/held".parse().unwrap(),
                    target: key,
                }],
            },
            TransferOptions::default(),
        )
        .await
    });

    reached.notified().await;
    assert_eq!(
        destination
            .try_collect()
            .await
            .unwrap()
            .removed
            .logical_objects,
        0
    );
    resume.notify_one();
    transfer.await.unwrap().unwrap();
    assert!(destination.try_collect().await.is_ok());
}

#[tokio::test]
async fn interrupted_payload_copy_keeps_roots_unchanged_and_retry_completes() {
    let source_repository = repository();
    let mutation = source_repository.mutation_session().await.unwrap();
    let one = mutation.stage_blob(b"first payload").await.unwrap();
    let one_key = one.record().key().clone();
    let two = mutation.stage_blob(b"second payload").await.unwrap();
    let two_key = two.record().key().clone();
    mutation.publish_unrooted(vec![one, two]).await.unwrap();
    drop(mutation);

    let source = FailingReadTransferSource {
        payloads: OneShotFailingReadStore {
            inner: source_repository.payloads().clone(),
            opens: Arc::new(AtomicUsize::new(0)),
            fail_at: Arc::new(AtomicUsize::new(2)),
        },
        repository: source_repository,
    };
    let destination = Repository::with_formats(
        MemoryBlobStore::new(),
        MemoryMetadataStore::new().unwrap(),
        FormatRegistry::builtin(),
        FormatLimits {
            max_batch_objects: 1,
            ..FormatLimits::default()
        },
    );
    let request = TransferRequest {
        objects: vec![
            ObjectRequest {
                key: one_key.clone(),
                recursive: true,
            },
            ObjectRequest {
                key: two_key.clone(),
                recursive: true,
            },
        ],
        roots: vec![DestinationRoot {
            name: "copies/payload-retry".parse().unwrap(),
            target: two_key.clone(),
        }],
    };

    assert!(matches!(
        transfer(&source, &destination, request.clone(), TransferOptions::default()).await,
        Err(TransferError::SourcePayload { key, error })
            if key == two_key && error.to_string().contains("injected payload read failure")
    ));
    let interrupted = destination.metadata().snapshot().await.unwrap();
    assert!(interrupted.object(&one_key).await.unwrap().is_some());
    assert!(interrupted.object(&two_key).await.unwrap().is_none());
    assert!(
        interrupted
            .root(&RootName::try_from("copies/payload-retry").unwrap())
            .await
            .unwrap()
            .is_none()
    );
    drop(interrupted);

    let result = transfer(&source, &destination, request, TransferOptions::default())
        .await
        .unwrap();
    assert_eq!(result.progress.published_objects, 1);
    assert_eq!(
        destination
            .metadata()
            .snapshot()
            .await
            .unwrap()
            .root(&RootName::try_from("copies/payload-retry").unwrap())
            .await
            .unwrap(),
        Some(two_key)
    );
}
