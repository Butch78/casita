use std::cell::RefCell;
use std::collections::HashMap;
use std::hint::black_box;
use std::time::Instant;

use casita::experimental::{CasitaGixOdb, CasitaGixOdbOptions, GitObjectFormat, Repository};
use gix::objs::{Find, FindHeader, Write};
use serde_json::{Value, json};

const SAMPLE_PREFIX: &str = "gix_odb_sample ";

#[derive(Default)]
struct MemoryOdb(RefCell<HashMap<gix::ObjectId, (gix::objs::Kind, Vec<u8>)>>);

impl Write for MemoryOdb {
    fn write_buf_with_known_id(
        &self,
        kind: gix::objs::Kind,
        from: &[u8],
        id: gix::ObjectId,
    ) -> Result<gix::ObjectId, gix::objs::write::Error> {
        let actual = gix::objs::compute_hash(gix::hash::Kind::Sha1, kind, from)?;
        assert_eq!(actual, id);
        self.0.borrow_mut().insert(id, (kind, from.to_vec()));
        Ok(id)
    }

    fn write_stream(
        &self,
        kind: gix::objs::Kind,
        _size: u64,
        from: &mut dyn std::io::Read,
    ) -> Result<gix::ObjectId, gix::objs::write::Error> {
        let mut body = Vec::new();
        from.read_to_end(&mut body)?;
        let id = gix::objs::compute_hash(gix::hash::Kind::Sha1, kind, &body)?;
        self.0.borrow_mut().insert(id, (kind, body));
        Ok(id)
    }

    fn write_stream_with_known_id(
        &self,
        kind: gix::objs::Kind,
        _size: u64,
        from: &mut dyn std::io::Read,
        id: gix::ObjectId,
    ) -> Result<gix::ObjectId, gix::objs::write::Error> {
        let mut body = Vec::new();
        from.read_to_end(&mut body)?;
        self.write_buf_with_known_id(kind, &body, id)
    }
}

impl Find for MemoryOdb {
    fn try_find<'a>(
        &self,
        id: &gix::oid,
        buffer: &'a mut Vec<u8>,
    ) -> Result<Option<gix::objs::Data<'a>>, gix::objs::find::Error> {
        let objects = self.0.borrow();
        let Some((kind, body)) = objects.get(id) else {
            return Ok(None);
        };
        buffer.clear();
        buffer.extend_from_slice(body);
        Ok(Some(gix::objs::Data {
            kind: *kind,
            object_hash: id.kind(),
            data: buffer,
        }))
    }
}

impl FindHeader for MemoryOdb {
    fn try_header(
        &self,
        id: &gix::oid,
    ) -> Result<Option<gix::objs::Header>, gix::objs::find::Error> {
        Ok(self
            .0
            .borrow()
            .get(id)
            .map(|(kind, body)| gix::objs::Header {
                kind: *kind,
                size: body.len() as u64,
            }))
    }
}

fn setting(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .map(|value| {
            value
                .parse()
                .unwrap_or_else(|_| panic!("{name} must be an integer"))
        })
        .unwrap_or(default)
}

fn bodies(objects: usize, body_bytes: usize) -> Vec<Vec<u8>> {
    (0..objects)
        .map(|index| {
            let mut body = vec![0; body_bytes];
            for (position, byte) in body.iter_mut().enumerate() {
                *byte = ((index.wrapping_mul(131) + position.wrapping_mul(17)) % 251) as u8;
            }
            body[..8].copy_from_slice(&(index as u64).to_le_bytes());
            body
        })
        .collect()
}

fn measure(
    implementation: &str,
    backend: &str,
    operation: &str,
    operations: usize,
    logical_bytes: u64,
    run: impl FnOnce(),
) -> Value {
    let started = Instant::now();
    run();
    let wall_nanos = started.elapsed().as_nanos() as u64;
    json!({
        "implementation": implementation,
        "backend": backend,
        "operation": operation,
        "operations": operations,
        "logical_bytes": logical_bytes,
        "wall_nanos": wall_nanos,
        "wall_seconds": wall_nanos as f64 / 1_000_000_000.0,
        "nanos_per_op": wall_nanos as f64 / operations as f64,
        "throughput_bytes_per_second": if logical_bytes == 0 { 0.0 } else { logical_bytes as f64 * 1_000_000_000.0 / wall_nanos as f64 },
        "status": "ok",
    })
}

fn pack_metrics(stats: casita::experimental::PackReadStats) -> Value {
    json!({
        "chunk_range_requests": stats.chunk_range_requests,
        "chunk_range_bytes": stats.chunk_range_bytes,
        "whole_pack_requests": stats.whole_pack_requests,
        "whole_pack_bytes": stats.whole_pack_bytes,
        "cache_hits": stats.cache_hits,
        "cache_promotions": stats.cache_promotions,
        "cache_evictions": stats.cache_evictions,
    })
}

fn validate<T: Find>(odb: &T, ids: &[gix::ObjectId], expected: &[Vec<u8>]) {
    let mut buffer = Vec::new();
    for (id, body) in ids.iter().zip(expected) {
        let found = odb.try_find(id, &mut buffer).unwrap().unwrap();
        assert_eq!(found.kind, gix::objs::Kind::Blob);
        assert_eq!(found.data, body);
    }
}

fn emit(
    mut samples: Vec<Value>,
    objects: usize,
    body_bytes: usize,
    oid_record_cache_capacity: usize,
) {
    for sample in &mut samples {
        sample["objects"] = json!(objects);
        sample["body_bytes"] = json!(body_bytes);
        sample["oid_record_cache_capacity"] = json!(oid_record_cache_capacity);
        println!("{SAMPLE_PREFIX}{sample}");
    }
}

fn main() {
    let objects = setting("CASITA_GIX_ODB_OBJECTS", 512);
    let body_bytes = setting("CASITA_GIX_ODB_BODY_BYTES", 4096);
    let header_operations = setting("CASITA_GIX_ODB_HEADER_OPERATIONS", 10_000);
    let miss_operations = setting("CASITA_GIX_ODB_MISS_OPERATIONS", 2_000);
    let pack_cache_bytes = setting("CASITA_GIX_ODB_PACK_CACHE_BYTES", 64 * 1024 * 1024) as u64;
    assert!(objects > 0);
    assert!(body_bytes >= 8);
    let bodies = bodies(objects, body_bytes);
    let logical_bytes = (objects * body_bytes) as u64;
    let mut samples = Vec::new();

    let memory = MemoryOdb::default();
    let mut memory_ids = Vec::with_capacity(objects);
    samples.push(measure(
        "gix",
        "memory",
        "write",
        objects,
        logical_bytes,
        || {
            for body in &bodies {
                memory_ids.push(memory.write_buf(gix::objs::Kind::Blob, body).unwrap());
            }
        },
    ));
    let mut buffer = Vec::new();
    samples.push(measure(
        "gix",
        "memory",
        "find",
        objects,
        logical_bytes,
        || {
            for id in &memory_ids {
                black_box(
                    memory
                        .try_find(id, &mut buffer)
                        .unwrap()
                        .unwrap()
                        .data
                        .len(),
                );
            }
        },
    ));
    samples.push(measure(
        "gix",
        "memory",
        "header",
        header_operations,
        0,
        || {
            for index in 0..header_operations {
                black_box(FindHeader::try_header(&memory, &memory_ids[index % objects]).unwrap());
            }
        },
    ));

    let mut options = CasitaGixOdbOptions::new(GitObjectFormat::Sha1);
    options.cache_objects = setting("CASITA_GIX_ODB_CACHE_OBJECTS", options.cache_objects);
    let oid_record_cache_capacity = options.cache_objects;
    let repository = Repository::memory().unwrap();
    let casita = CasitaGixOdb::with_options(repository.clone(), options.clone()).unwrap();
    let mut casita_ids = Vec::with_capacity(objects);
    samples.push(measure(
        "casita",
        "memory",
        "write",
        objects,
        logical_bytes,
        || {
            for body in &bodies {
                casita_ids.push(casita.write_buf(gix::objs::Kind::Blob, body).unwrap());
            }
            casita.flush().unwrap();
        },
    ));
    assert_eq!(casita_ids, memory_ids);
    samples.push(measure(
        "casita",
        "memory",
        "cold-find",
        objects,
        logical_bytes,
        || {
            for id in &casita_ids {
                black_box(
                    casita
                        .try_find(id, &mut buffer)
                        .unwrap()
                        .unwrap()
                        .data
                        .len(),
                );
            }
        },
    ));
    samples.push(measure(
        "casita",
        "memory",
        "warm-find",
        objects,
        logical_bytes,
        || {
            for id in &casita_ids {
                black_box(
                    casita
                        .try_find(id, &mut buffer)
                        .unwrap()
                        .unwrap()
                        .data
                        .len(),
                );
            }
        },
    ));
    samples.push(measure(
        "casita",
        "memory",
        "header",
        header_operations,
        0,
        || {
            for index in 0..header_operations {
                black_box(FindHeader::try_header(&casita, &casita_ids[index % objects]).unwrap());
            }
        },
    ));
    let mut missing_bytes = [0xa5; 20];
    let mut missing = gix::ObjectId::from_bytes_or_panic(&missing_bytes);
    while casita_ids.contains(&missing) {
        missing_bytes[0] = missing_bytes[0].wrapping_add(1);
        missing = gix::ObjectId::from_bytes_or_panic(&missing_bytes);
    }
    samples.push(measure(
        "casita",
        "memory",
        "missing-header",
        miss_operations,
        0,
        || {
            for _ in 0..miss_operations {
                black_box(FindHeader::try_header(&casita, &missing).unwrap());
            }
        },
    ));
    let missing_ids: Vec<_> = (0..miss_operations)
        .map(|index| {
            let mut bytes = [0xa6; 20];
            bytes[..8].copy_from_slice(&(index as u64).to_le_bytes());
            let id = gix::ObjectId::from_bytes_or_panic(&bytes);
            assert!(!casita_ids.contains(&id));
            id
        })
        .collect();
    samples.push(measure(
        "casita",
        "memory",
        "distinct-missing-header",
        miss_operations,
        0,
        || {
            for id in &missing_ids {
                assert!(FindHeader::try_header(&casita, id).unwrap().is_none());
            }
        },
    ));
    validate(&casita, &casita_ids, &bodies);
    drop(casita);

    let native_root = tempfile::tempdir().unwrap();
    let native = gix::init_bare(native_root.path()).unwrap();
    let mut native_ids = Vec::with_capacity(objects);
    samples.push(measure(
        "gix",
        "local",
        "write",
        objects,
        logical_bytes,
        || {
            for body in &bodies {
                native_ids.push(native.write_buf(gix::objs::Kind::Blob, body).unwrap());
            }
        },
    ));
    samples.push(measure(
        "gix",
        "local",
        "cold-find",
        objects,
        logical_bytes,
        || {
            for id in &native_ids {
                black_box(
                    native
                        .try_find(id, &mut buffer)
                        .unwrap()
                        .unwrap()
                        .data
                        .len(),
                );
            }
        },
    ));
    samples.push(measure(
        "gix",
        "local",
        "warm-find",
        objects,
        logical_bytes,
        || {
            for id in &native_ids {
                black_box(
                    native
                        .try_find(id, &mut buffer)
                        .unwrap()
                        .unwrap()
                        .data
                        .len(),
                );
            }
        },
    ));
    samples.push(measure(
        "gix",
        "local",
        "header",
        header_operations,
        0,
        || {
            for index in 0..header_operations {
                black_box(FindHeader::try_header(&native, &native_ids[index % objects]).unwrap());
            }
        },
    ));
    validate(&native, &native_ids, &bodies);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let local_root = tempfile::tempdir().unwrap();
    let local_repository = runtime
        .block_on(Repository::local_with_pack_options(
            local_root.path(),
            casita::experimental::PackOptions {
                target_size: casita::experimental::DEFAULT_LOCAL_PACK_TARGET_SIZE,
                cache_capacity: pack_cache_bytes,
            },
        ))
        .unwrap();
    let local = CasitaGixOdb::with_options(local_repository.clone(), options.clone()).unwrap();
    let mut local_ids = Vec::with_capacity(objects);
    samples.push(measure(
        "casita",
        "local",
        "write",
        objects,
        logical_bytes,
        || {
            for body in &bodies {
                local_ids.push(local.write_buf(gix::objs::Kind::Blob, body).unwrap());
            }
            local.flush().unwrap();
        },
    ));
    assert_eq!(local_ids, native_ids);
    local_repository.payloads().reset_pack_read_stats();
    let mut cold = measure(
        "casita",
        "local",
        "cold-find",
        objects,
        logical_bytes,
        || {
            for id in &local_ids {
                black_box(local.try_find(id, &mut buffer).unwrap().unwrap().data.len());
            }
        },
    );
    cold["pack"] = pack_metrics(local_repository.payloads().pack_read_stats().unwrap());
    samples.push(cold);
    local_repository.payloads().reset_pack_read_stats();
    let mut warm = measure(
        "casita",
        "local",
        "warm-find",
        objects,
        logical_bytes,
        || {
            for id in &local_ids {
                black_box(local.try_find(id, &mut buffer).unwrap().unwrap().data.len());
            }
        },
    );
    warm["pack"] = pack_metrics(local_repository.payloads().pack_read_stats().unwrap());
    samples.push(warm);
    samples.push(measure(
        "casita",
        "local",
        "header",
        header_operations,
        0,
        || {
            for index in 0..header_operations {
                black_box(FindHeader::try_header(&local, &local_ids[index % objects]).unwrap());
            }
        },
    ));
    validate(&local, &local_ids, &bodies);
    drop(local);
    drop(local_repository);

    let reopened_repository = runtime
        .block_on(Repository::local_with_pack_options(
            local_root.path(),
            casita::experimental::PackOptions {
                target_size: casita::experimental::DEFAULT_LOCAL_PACK_TARGET_SIZE,
                cache_capacity: pack_cache_bytes,
            },
        ))
        .unwrap();
    let reopened = CasitaGixOdb::with_options(reopened_repository, options).unwrap();
    samples.push(measure(
        "casita",
        "local",
        "reopened-first-header",
        1,
        0,
        || {
            assert!(
                FindHeader::try_header(&reopened, &local_ids[0])
                    .unwrap()
                    .is_some()
            );
        },
    ));
    samples.push(measure(
        "casita",
        "local",
        "reopened-header",
        objects,
        0,
        || {
            for id in &local_ids {
                black_box(FindHeader::try_header(&reopened, id).unwrap());
            }
        },
    ));
    validate(&reopened, &local_ids, &bodies);

    emit(samples, objects, body_bytes, oid_record_cache_capacity);
}
