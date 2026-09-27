//! Snapshot-local lookup over shared immutable read caches.
use super::*;

const MAX_CONCURRENT_LOCATION_LOOKUPS: usize = 16;

/// Only immutable bytes belong here. Visibility, tombstones and selected
/// generations belong to the catalog view, never to this shared cache.
pub(super) struct SharedReadCaches {
    pub(super) catalog_shard_cache: StdMutex<CatalogShardCache>,
    pub(super) catalog_shard_loads: [Mutex<()>; PACK_CACHE_LOAD_STRIPES],
}

impl SharedReadCaches {
    pub(super) fn new() -> Self {
        Self {
            catalog_shard_cache: StdMutex::new(CatalogShardCache::new(CATALOG_SHARD_CACHE_BYTES)),
            catalog_shard_loads: std::array::from_fn(|_| Mutex::new(())),
        }
    }
}

/// Lookup state selected by one catalog. Lazy legacy runs are materialized
/// once; all other state remains immutable for the snapshot's lifetime.
struct CatalogLookup {
    reader: PackReader,
    index: Index,
    lazy: LazyCatalogOverlay,
    materialized: tokio::sync::OnceCell<(Index, LazyCatalogOverlay)>,
}

/// Retain only the most recently selected immutable catalog, never its lease.
/// This lives on the backend, outside SharedReadCaches, so the lookup's reader
/// cannot form a reference cycle through its own caches.
#[derive(Default)]
pub(super) struct CatalogLookupCache {
    current: Mutex<Option<(Vec<u8>, Arc<CatalogLookup>)>>,
}

impl CatalogLookup {
    async fn location(&self, digest: &ChunkId) -> io::Result<Option<Location>> {
        let (index, lazy) = if let Some(state) = self.materialized.get() {
            (&state.0, &state.1)
        } else {
            (&self.index, &self.lazy)
        };
        if let Some(location) = index.chunks.get(digest) {
            return Ok(Some(location));
        }
        if lazy
            .run_refs
            .values()
            .any(|reference| reference.query.is_none())
        {
            let (index, lazy) = self
                .materialized
                .get_or_try_init(|| async {
                    let (index, lazy, _) = self.reader.materialize_runs(&self.lazy).await?;
                    Ok::<_, io::Error>((index, lazy))
                })
                .await?;
            if let Some(location) = index.chunks.get(digest) {
                return Ok(Some(location));
            }
            return Ok(self
                .reader
                .base_locations(lazy, digest)
                .await?
                .into_iter()
                .next());
        }
        Ok(self
            .reader
            .base_locations(lazy, digest)
            .await?
            .into_iter()
            .next())
    }
}

/// The selected lookup state and its protecting lease. No writer backend is
/// constructed or retained to open a snapshot.
pub(crate) struct CatalogSnapshot {
    lookup: Arc<CatalogLookup>,
    storage: PackReader,
    pin: crate::metadata::DataPinLease,
}

impl CatalogSnapshot {
    pub(super) async fn open(
        storage: PackReader,
        catalog: &[u8],
        pin: crate::metadata::DataPinLease,
        cache: &CatalogLookupCache,
    ) -> io::Result<Self> {
        // Serialize cold admission, including concurrent readers of one catalog.
        // Compare the complete protected catalog, not just its generation.
        let mut cached = cache.current.lock().await;
        if let Some((selected, lookup)) = cached.as_ref()
            && selected == catalog
        {
            return Ok(Self {
                lookup: lookup.clone(),
                storage,
                pin,
            });
        }
        let mut reader = storage.clone();
        reader.read_counters = Arc::default();
        reader.catalog_run_indexes = Arc::default();
        let loaded = reader
            .decode_v1_index_catalog(
                catalog,
                UpdateVersion {
                    e_tag: None,
                    version: None,
                },
            )
            .await?;
        let index = loaded.index.ok_or_else(|| {
            io::Error::other("state-committed pack catalog could not be reconstructed")
        })?;
        let lookup = Arc::new(CatalogLookup {
            reader,
            index,
            lazy: loaded.lazy,
            materialized: tokio::sync::OnceCell::new(),
        });
        *cached = Some((catalog.to_vec(), lookup.clone()));
        Ok(Self {
            lookup,
            storage,
            pin,
        })
    }

    pub(crate) fn manifest_definitely_absent(&self, digest: &BlobId) -> bool {
        let (index, lazy) = self
            .lookup
            .materialized
            .get()
            .map_or((&self.lookup.index, &self.lookup.lazy), |state| {
                (&state.0, &state.1)
            });
        self.lookup
            .reader
            .manifest_definitely_absent(index, lazy, digest)
    }

    /// Bare chunks are fully buffered while the snapshot lease is still held.
    pub(crate) async fn read_bare_chunk(&self, digest: &ChunkId) -> io::Result<Option<Bytes>> {
        match self.lookup.location(digest).await? {
            Some(location) => Ok(Some(self.storage.read_location(*digest, location).await?)),
            None => Ok(None),
        }
    }

    #[cfg(test)]
    pub(super) fn counters(&self) -> Arc<PackReadCounters> {
        self.lookup.reader.read_counters.clone()
    }

    pub(crate) async fn prepare_read(self, chunks: &[ChunkMeta]) -> io::Result<PinnedReadPlan> {
        let ids = chunks
            .iter()
            .map(|chunk| chunk.digest)
            .collect::<BTreeSet<_>>();
        let locations = futures::stream::iter(ids.into_iter().map(|digest| {
            let lookup = &self.lookup;
            async move {
                let location = lookup.location(&digest).await?.ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("missing chunk {digest} in object read"),
                    )
                })?;
                Ok::<_, io::Error>((digest, location))
            }
        }))
        .buffer_unordered(MAX_CONCURRENT_LOCATION_LOOKUPS)
        .try_collect::<BTreeMap<_, _>>()
        .await?;
        let mut resources = BTreeSet::new();
        for (digest, location) in &locations {
            resources.insert(crate::metadata::PinResource::Chunk(*digest));
            resources.insert(crate::metadata::PinResource::StorageObject(
                pack_path(&self.storage.base, &location.pack).to_string(),
            ));
        }
        self.pin
            .protect(resources)
            .await
            .map_err(io::Error::other)?;
        Ok(PinnedReadPlan {
            storage: self.storage,
            locations,
            _pin: self.pin,
        })
    }
}

/// Resolved locations and their lease need only an immutable pack reader.
pub(crate) struct PinnedReadPlan {
    storage: PackReader,
    locations: BTreeMap<ChunkId, Location>,
    _pin: crate::metadata::DataPinLease,
}

impl PinnedReadPlan {
    pub(crate) fn into_reader(
        self,
        chunks: Vec<ChunkMeta>,
        decode: crate::byte_budget::ByteBudget,
        expected: BlobId,
    ) -> super::super::chunked_reader::ChunkedReader {
        let frozen = self
            .locations
            .into_iter()
            .map(|(id, location)| (id, FrozenChunk(location)))
            .collect();
        fetch::reader(
            self.storage,
            chunks,
            frozen,
            Some(self._pin),
            decode,
            expected,
        )
    }

    #[cfg(test)]
    pub(crate) async fn read_chunk(&self, digest: &ChunkId) -> io::Result<Bytes> {
        let location = self
            .locations
            .get(digest)
            .ok_or_else(|| io::Error::other("chunk outside pinned read plan"))?;
        self.storage.read_location(*digest, *location).await
    }
}

/// Immutable object I/O, shared cache ownership and request accounting. No
/// staging, publication, collection or mutable catalog selection lives here.
#[derive(Clone)]
pub(super) struct PackReader {
    pub(super) object_store: Arc<dyn ObjectStore>,
    pub(super) base: Path,
    pub(super) fetch: Arc<fetch::State>,
    pub(super) read_caches: Arc<SharedReadCaches>,
    pub(super) read_counters: Arc<PackReadCounters>,
    pub(super) catalog_run_indexes: Arc<StdMutex<HashMap<Digest, Arc<CatalogRunQueryIndex>>>>,
}

impl PackReader {
    pub(super) async fn materialize_runs(
        &self,
        snapshot: &LazyCatalogOverlay,
    ) -> io::Result<(Index, LazyCatalogOverlay, BTreeMap<u8, CatalogRun>)> {
        let mut ordered = snapshot.run_refs.iter().collect::<Vec<_>>();
        ordered.sort_unstable_by_key(|(_, reference)| reference.first_generation);
        let mut loaded = BTreeMap::new();
        let mut overlay = Index {
            manifests_complete: true,
            ..Index::default()
        };
        let mut materialized = LazyCatalogOverlay {
            base: snapshot.base.clone(),
            ..LazyCatalogOverlay::default()
        };
        for (level, reference) in ordered {
            let run = self.load_catalog_run(reference.clone()).await?;
            let decoded = decode_index_delta(&run.delta)?;
            materialized.apply(&decoded);
            apply_decoded_index_delta(&mut overlay, decoded);
            loaded.insert(*level, run);
        }
        for delta in &snapshot.root_deltas {
            let decoded = decode_index_delta(delta)?;
            materialized.apply(&decoded);
            apply_decoded_index_delta(&mut overlay, decoded);
        }

        Ok((overlay, materialized, loaded))
    }

    pub(super) async fn load_catalog_shard_range(
        &self,
        reference: ShardRef,
        offset: u64,
        length: u64,
        digest: Digest,
    ) -> io::Result<Bytes> {
        if let Some(bytes) = self
            .read_caches
            .catalog_shard_cache
            .lock()
            .unwrap()
            .get(digest)
        {
            return Ok(bytes);
        }
        let stripe = usize::from(digest.as_bytes()[0]) % self.read_caches.catalog_shard_loads.len();
        let _load = self.read_caches.catalog_shard_loads[stripe].lock().await;
        if let Some(bytes) = self
            .read_caches
            .catalog_shard_cache
            .lock()
            .unwrap()
            .get(digest)
        {
            return Ok(bytes);
        }
        let end = offset
            .checked_add(length)
            .filter(|end| *end <= reference.encoded_bytes)
            .ok_or_else(|| io::Error::other("catalog shard range overflow"))?;
        self.read_counters
            .index_requests
            .fetch_add(1, Ordering::Relaxed);
        let bytes = self
            .object_store
            .get_range(
                &sharded_path(&self.base, INDEXES_KIND, &reference.digest),
                offset..end,
            )
            .await
            .map_err(object_store_io_error)?;
        self.read_counters
            .index_bytes
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        if bytes.len() as u64 != length || Digest::from(blake3::hash(&bytes)) != digest {
            return Err(io::Error::other("catalog shard range identity mismatch"));
        }
        self.read_caches
            .catalog_shard_cache
            .lock()
            .unwrap()
            .insert(digest, bytes.clone());
        Ok(bytes)
    }

    pub(super) async fn load_catalog_shard(&self, reference: ShardRef) -> io::Result<Bytes> {
        if let Some(bytes) = self
            .read_caches
            .catalog_shard_cache
            .lock()
            .unwrap()
            .get(reference.digest)
        {
            return Ok(bytes);
        }
        let stripe = usize::from(reference.digest.as_bytes()[0])
            % self.read_caches.catalog_shard_loads.len();
        let _load = self.read_caches.catalog_shard_loads[stripe].lock().await;
        if let Some(bytes) = self
            .read_caches
            .catalog_shard_cache
            .lock()
            .unwrap()
            .get(reference.digest)
        {
            return Ok(bytes);
        }
        self.read_counters
            .index_requests
            .fetch_add(1, Ordering::Relaxed);
        let bytes = self
            .object_store
            .get(&sharded_path(&self.base, INDEXES_KIND, &reference.digest))
            .await
            .map_err(object_store_io_error)?
            .bytes()
            .await
            .map_err(io::Error::other)?;
        self.read_counters
            .index_bytes
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        let started = Instant::now();
        let actual = Digest::from(blake3::hash(&bytes));
        self.read_counters.index_hash_nanos.fetch_add(
            u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        if bytes.len() as u64 != reference.encoded_bytes || actual != reference.digest {
            return Err(io::Error::other("catalog shard identity mismatch"));
        }
        self.read_caches
            .catalog_shard_cache
            .lock()
            .unwrap()
            .insert(reference.digest, bytes.clone());
        Ok(bytes)
    }

    pub(super) async fn load_catalog_run(
        &self,
        reference: CatalogRunRef,
    ) -> io::Result<CatalogRun> {
        self.read_counters
            .index_requests
            .fetch_add(1, Ordering::Relaxed);
        let bytes = self
            .object_store
            .get(&sharded_path(&self.base, INDEXES_KIND, &reference.digest))
            .await
            .map_err(object_store_io_error)?
            .bytes()
            .await
            .map_err(io::Error::other)?;
        self.read_counters
            .index_bytes
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        let started = Instant::now();
        let digest = Digest::from(blake3::hash(&bytes));
        self.read_counters.index_hash_nanos.fetch_add(
            u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        if bytes.len() as u64 != reference.encoded_bytes || digest != reference.digest {
            return Err(io::Error::other("catalog run identity mismatch"));
        }
        if let Some(expected) = &reference.query
            && !expected.routing.is_empty()
            && catalog_run_query_ref(&bytes)? != *expected
        {
            return Err(io::Error::other(
                "catalog run routing does not match the immutable object",
            ));
        }
        let started = Instant::now();
        let run = decode_catalog_run(&bytes)?;
        self.read_counters.index_decode_nanos.fetch_add(
            u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        if run.first_generation != reference.first_generation
            || run.last_generation != reference.last_generation
        {
            return Err(io::Error::other("catalog run generation mismatch"));
        }
        Ok(run)
    }

    pub(super) async fn load_catalog_run_query_index(
        &self,
        reference: &CatalogRunRef,
    ) -> io::Result<Option<Arc<CatalogRunQueryIndex>>> {
        let Some(query) = reference.query.as_ref() else {
            return Ok(None);
        };
        if let Some(index) = self
            .catalog_run_indexes
            .lock()
            .unwrap()
            .get(&reference.digest)
            .cloned()
        {
            return Ok(Some(index));
        }
        let index = Arc::new(decode_catalog_run_routing(query)?);
        self.catalog_run_indexes
            .lock()
            .unwrap()
            .insert(reference.digest, index.clone());
        Ok(Some(index))
    }

    pub(super) async fn lookup_catalog_run_chunk(
        &self,
        reference: &CatalogRunRef,
        index: &CatalogRunQueryIndex,
        digest: &ChunkId,
    ) -> io::Result<Option<Location>> {
        let Some(block) = index.chunk_block(digest) else {
            return Ok(None);
        };
        if let Some(bytes) = self
            .read_caches
            .catalog_shard_cache
            .lock()
            .unwrap()
            .get(block.digest)
        {
            return lookup_catalog_run_chunk_block(&bytes, block, digest);
        }
        let stripe =
            usize::from(block.digest.as_bytes()[0]) % self.read_caches.catalog_shard_loads.len();
        let _load = self.read_caches.catalog_shard_loads[stripe].lock().await;
        if let Some(bytes) = self
            .read_caches
            .catalog_shard_cache
            .lock()
            .unwrap()
            .get(block.digest)
        {
            return lookup_catalog_run_chunk_block(&bytes, block, digest);
        }
        let end = block
            .offset
            .checked_add(block.encoded_bytes)
            .ok_or_else(|| io::Error::other("catalog run block range overflow"))?;
        self.read_counters
            .index_requests
            .fetch_add(1, Ordering::Relaxed);
        let bytes = self
            .object_store
            .get_range(
                &sharded_path(&self.base, INDEXES_KIND, &reference.digest),
                block.offset..end,
            )
            .await
            .map_err(object_store_io_error)?;
        self.read_counters
            .index_bytes
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        let found = lookup_catalog_run_chunk_block(&bytes, block, digest)?;
        self.read_caches
            .catalog_shard_cache
            .lock()
            .unwrap()
            .insert(block.digest, bytes);
        Ok(found)
    }

    pub(super) async fn read_location(
        &self,
        digest: ChunkId,
        location: Location,
    ) -> io::Result<Bytes> {
        if let Some(bytes) = self.fetch.cache.lock().unwrap().get(digest) {
            self.read_counters
                .cache_hits
                .fetch_add(1, Ordering::Relaxed);
            return Ok(bytes);
        }

        let end = location
            .offset
            .checked_add(location.framed_len)
            .ok_or_else(|| io::Error::other("pack range overflow"))?;
        self.read_counters
            .chunk_range_requests
            .fetch_add(1, Ordering::Relaxed);
        let bytes = self
            .object_store
            .get_range(
                &pack_path(&self.base, &location.pack),
                Range {
                    start: location.offset,
                    end,
                },
            )
            .await
            .map_err(object_store_io_error)?;
        self.read_counters
            .chunk_range_bytes
            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
        if bytes.len() as u64 != location.framed_len {
            return Err(io::Error::other("short pack range read"));
        }
        let evictions = self.fetch.cache.lock().unwrap().insert(digest, &bytes);
        self.read_counters
            .cache_evictions
            .fetch_add(evictions, Ordering::Relaxed);
        Ok(bytes)
    }

    pub(super) async fn decode_v1_index_catalog(
        &self,
        catalog: &[u8],
        version: UpdateVersion,
    ) -> io::Result<LoadedIndexCatalog> {
        let external = external::ExternalCatalog::decode(catalog)?;
        let resolved = if external.is_some() {
            Some(self.resolve_state_catalog(catalog).await?)
        } else {
            None
        };
        let catalog = resolved.as_deref().unwrap_or(catalog);
        self.read_counters
            .index_bytes
            .fetch_add(catalog.len() as u64, Ordering::Relaxed);
        let started = Instant::now();
        let root = match decode_delta_catalog(catalog) {
            Ok(root) => root,
            Err(_) => {
                return Ok(LoadedIndexCatalog {
                    index: None,
                    witness: IndexCatalogWitness {
                        version: Some(version),
                        pointer_digest: Some(blake3::hash(catalog).into()),
                        ..IndexCatalogWitness::default()
                    },
                    lazy: LazyCatalogOverlay::default(),
                });
            }
        };
        self.read_counters.index_hash_nanos.fetch_add(
            u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        let mut witness = IndexCatalogWitness {
            external,
            version: Some(version),
            pointer_digest: Some(blake3::hash(catalog).into()),
            generation: root.generation,
            root: Some(root.clone()),
            runs: BTreeMap::new(),
            prepared_rebase: None,
            prepared_map: None,
        };
        let (mut index, mut lazy) = match &root.base {
            CatalogBase::Inline(checkpoint) => {
                let started = Instant::now();
                let decoded = decode_index_checkpoint_without_inventory(checkpoint);
                self.read_counters.index_decode_nanos.fetch_add(
                    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                    Ordering::Relaxed,
                );
                let Ok(index) = decoded else {
                    return Ok(LoadedIndexCatalog {
                        index: None,
                        witness,
                        lazy: LazyCatalogOverlay::default(),
                    });
                };
                if !index.manifests_complete {
                    return Ok(LoadedIndexCatalog {
                        index: None,
                        witness,
                        lazy: LazyCatalogOverlay::default(),
                    });
                }
                (index, LazyCatalogOverlay::default())
            }
            CatalogBase::Checkpoint(expected) => {
                self.read_counters
                    .index_requests
                    .fetch_add(1, Ordering::Relaxed);
                let object = match self
                    .object_store
                    .get(&sharded_path(&self.base, INDEXES_KIND, expected))
                    .await
                {
                    Ok(object) => object,
                    Err(object_store::Error::NotFound { .. }) => {
                        return Ok(LoadedIndexCatalog {
                            index: None,
                            witness,
                            lazy: LazyCatalogOverlay::default(),
                        });
                    }
                    Err(error) => return Err(io::Error::other(error)),
                };
                let checkpoint = object.bytes().await.map_err(io::Error::other)?;
                self.read_counters
                    .index_bytes
                    .fetch_add(checkpoint.len() as u64, Ordering::Relaxed);
                let started = Instant::now();
                let actual = Digest::from(blake3::hash(&checkpoint));
                self.read_counters.index_hash_nanos.fetch_add(
                    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                    Ordering::Relaxed,
                );
                if actual != *expected {
                    return Ok(LoadedIndexCatalog {
                        index: None,
                        witness,
                        lazy: LazyCatalogOverlay::default(),
                    });
                }
                let started = Instant::now();
                let decoded = decode_index_checkpoint_without_inventory(&checkpoint);
                self.read_counters.index_decode_nanos.fetch_add(
                    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                    Ordering::Relaxed,
                );
                let Ok(index) = decoded else {
                    return Ok(LoadedIndexCatalog {
                        index: None,
                        witness,
                        lazy: LazyCatalogOverlay::default(),
                    });
                };
                if !index.manifests_complete {
                    return Ok(LoadedIndexCatalog {
                        index: None,
                        witness,
                        lazy: LazyCatalogOverlay::default(),
                    });
                }
                (index, LazyCatalogOverlay::default())
            }
            CatalogBase::Sharded { root, shard_bits } => {
                self.read_counters
                    .index_requests
                    .fetch_add(1, Ordering::Relaxed);
                let map_bytes = match self
                    .object_store
                    .get(&sharded_path(&self.base, INDEXES_KIND, root))
                    .await
                {
                    Ok(object) => object.bytes().await.map_err(io::Error::other)?,
                    Err(object_store::Error::NotFound { .. }) => {
                        return Ok(LoadedIndexCatalog {
                            index: None,
                            witness,
                            lazy: LazyCatalogOverlay::default(),
                        });
                    }
                    Err(error) => return Err(io::Error::other(error)),
                };
                self.read_counters
                    .index_bytes
                    .fetch_add(map_bytes.len() as u64, Ordering::Relaxed);
                let started = Instant::now();
                let actual = Digest::from(blake3::hash(&map_bytes));
                self.read_counters.index_hash_nanos.fetch_add(
                    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                    Ordering::Relaxed,
                );
                let map = decode_shard_map(&map_bytes);
                let Ok(map) = map else {
                    return Ok(LoadedIndexCatalog {
                        index: None,
                        witness,
                        lazy: LazyCatalogOverlay::default(),
                    });
                };
                if actual != *root || map.shard_bits != *shard_bits {
                    return Ok(LoadedIndexCatalog {
                        index: None,
                        witness,
                        lazy: LazyCatalogOverlay::default(),
                    });
                }
                let index = Index {
                    manifests_complete: true,
                    ..Index::default()
                };
                (
                    index,
                    LazyCatalogOverlay {
                        base: Some(ShardedIndexBase { map: Arc::new(map) }),
                        ..LazyCatalogOverlay::default()
                    },
                )
            }
        };

        let defer_runs = lazy.base.is_some() && !root.runs.is_empty();
        if defer_runs {
            let mut run_refs = root.runs.clone();
            let routing = &lazy
                .base
                .as_ref()
                .expect("sharded catalog base")
                .map
                .run_routing;
            for reference in run_refs.values_mut() {
                let Some(query) = reference.query.as_mut() else {
                    continue;
                };
                match (query.routing.is_empty(), routing.get(&reference.digest)) {
                    (true, Some(external)) => query.routing = external.clone(),
                    (true, None) => {
                        return Err(io::Error::other(
                            "sharded catalog map is missing run routing",
                        ));
                    }
                    (false, Some(external)) if external != &query.routing => {
                        return Err(io::Error::other(
                            "catalog root and shard map disagree on run routing",
                        ));
                    }
                    _ => {}
                }
            }
            if routing.keys().any(|digest| {
                !run_refs
                    .values()
                    .any(|reference| reference.digest == *digest)
            }) {
                return Err(io::Error::other(
                    "sharded catalog map contains orphan run routing",
                ));
            }
            lazy.run_refs = run_refs;
            lazy.root_deltas = root.deltas.clone();
        } else {
            let mut run_refs = root.runs.iter().collect::<Vec<_>>();
            run_refs.sort_unstable_by_key(|(_, run)| run.first_generation);
            for (level, reference) in run_refs {
                let run = match self.load_catalog_run(reference.clone()).await {
                    Ok(run) => run,
                    Err(_) => {
                        return Ok(LoadedIndexCatalog {
                            index: None,
                            witness,
                            lazy: LazyCatalogOverlay::default(),
                        });
                    }
                };
                let decoded = decode_index_delta(&run.delta)?;
                lazy.apply(&decoded);
                apply_decoded_index_delta(&mut index, decoded);
                witness.runs.insert(*level, run);
            }
        }

        let started = Instant::now();
        for delta in &root.deltas {
            let decoded = decode_index_delta(delta);
            let Ok(decoded) = decoded else {
                return Ok(LoadedIndexCatalog {
                    index: None,
                    witness,
                    lazy: LazyCatalogOverlay::default(),
                });
            };
            lazy.apply(&decoded);
            apply_decoded_index_delta(&mut index, decoded);
        }
        self.read_counters.index_decode_nanos.fetch_add(
            u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        Ok(LoadedIndexCatalog {
            index: Some(index),
            witness,
            lazy,
        })
    }

    pub(super) async fn base_locations(
        &self,
        lazy: &LazyCatalogOverlay,
        digest: &ChunkId,
    ) -> io::Result<Vec<Location>> {
        let mut newer_runs = Vec::new();
        if !lazy.run_refs.is_empty() {
            let mut runs = lazy.run_refs.values().cloned().collect::<Vec<_>>();
            runs.sort_unstable_by_key(|reference| std::cmp::Reverse(reference.last_generation));
            for reference in runs {
                let index = self
                    .load_catalog_run_query_index(&reference)
                    .await?
                    .expect("queryable catalog run has an index reference");
                if let Some(location) = self
                    .lookup_catalog_run_chunk(&reference, &index, digest)
                    .await?
                    && !lazy.changed_packs.contains(&location.pack)
                    && newer_runs.iter().all(|newer: &Arc<CatalogRunQueryIndex>| {
                        !newer.changes_pack(&location.pack)
                    })
                {
                    return Ok(vec![location]);
                }
                newer_runs.push(index);
            }
        }
        let Some(base) = &lazy.base else {
            return Ok(Vec::new());
        };
        let prefix = digest_prefix(digest.as_digest(), base.map.shard_bits)?;
        let Ok(at) = base
            .map
            .chunks
            .binary_search_by_key(&prefix, |shard| shard.prefix)
        else {
            return Ok(Vec::new());
        };
        let reference = base.map.chunks[at];
        let locations = if let Some((routing_digest, routing_len)) = reference
            .routing
            .filter(|_| reference.encoded_bytes > 128 * 1024)
        {
            let cached = self
                .read_caches
                .catalog_shard_cache
                .lock()
                .unwrap()
                .get_routing(routing_digest);
            let blocks = match cached {
                Some(blocks) => blocks,
                None => {
                    let routing = self
                        .load_catalog_shard_range(
                            reference,
                            reference.encoded_bytes - routing_len,
                            routing_len,
                            routing_digest,
                        )
                        .await?;
                    let blocks = Arc::new(shard::decode_chunk_routing(&routing, reference)?);
                    self.read_caches
                        .catalog_shard_cache
                        .lock()
                        .unwrap()
                        .insert_with_routing(routing_digest, routing, Some(blocks.clone()));
                    blocks
                }
            };
            let mut locations = Vec::new();
            let start = blocks.partition_point(|block| block.last < *digest.as_digest());
            for block in blocks[start..]
                .iter()
                .take_while(|block| block.first <= *digest.as_digest())
            {
                let bytes = self
                    .load_catalog_shard_range(reference, block.offset, block.length, block.digest)
                    .await?;
                locations.extend(shard::lookup_chunk_block(
                    &bytes,
                    base.map.shard_bits,
                    prefix,
                    digest,
                )?);
            }
            locations
        } else {
            let bytes = self.load_catalog_shard(reference).await?;
            lookup_chunk_locations_shard(&bytes, prefix, digest)?
        };
        Ok(locations
            .into_iter()
            .filter(|location| {
                !lazy.changed_packs.contains(&location.pack)
                    && newer_runs
                        .iter()
                        .all(|run| !run.changes_pack(&location.pack))
            })
            .collect())
    }
}

impl PackReader {
    pub(super) fn manifest_definitely_absent(
        &self,
        index: &Index,
        lazy: &LazyCatalogOverlay,
        digest: &BlobId,
    ) -> bool {
        if index.manifests.contains(digest) {
            return false;
        }
        if lazy.removed_manifests.contains(digest) {
            return true;
        }
        if !lazy.run_refs.is_empty() {
            // The synchronous fast path cannot issue remote I/O. Until the
            // run overlay is materialized, absence is deliberately
            // inconclusive rather than risking a false negative.
            return false;
        }
        let Some(base) = &lazy.base else {
            return index.manifests_complete;
        };
        let Ok(prefix) = digest_prefix(digest.as_digest(), base.map.shard_bits) else {
            return false;
        };
        let Ok(at) = base
            .map
            .manifests
            .binary_search_by_key(&prefix, |shard| shard.prefix)
        else {
            return true;
        };
        let reference = base.map.manifests[at];
        self.read_caches
            .catalog_shard_cache
            .lock()
            .unwrap()
            .get(reference.digest)
            .and_then(|bytes| manifest_shard_contains(&bytes, prefix, digest).ok())
            .is_some_and(|contains| !contains)
    }
}
