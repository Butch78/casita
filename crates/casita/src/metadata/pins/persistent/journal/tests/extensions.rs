use super::*;

fn extension_body(state: &PinInventory, pins: BTreeMap<PinToken, DataPin>) -> Vec<u8> {
    let mut next = state.clone();
    next.revision += 1;
    let mut bytes = Changes::new(state.revision).encode(&next).unwrap();
    bytes.extend(
        codec::encode(&PinInventory {
            pins,
            ..Default::default()
        })
        .unwrap(),
    );
    bytes
}

#[test]
fn extension_frames_reject_truncation_invalid_targets_and_replacement_fields() {
    let token = PinToken::fresh().unwrap();
    let state = PinInventory {
        revision: 1,
        pins: BTreeMap::from([(token.clone(), staging("old"))]),
        ..Default::default()
    };
    let pins = BTreeMap::from([(token.clone(), staging("new"))]);
    let bytes = extension_body(&state, pins.clone());
    let mut replayed = state.clone();
    let (touched, globals) = apply(&mut replayed, &bytes, EXTENSION_FRAME).unwrap();
    let mut index = Index::new(&state).unwrap();
    index.update(globals, &touched, &replayed).unwrap();
    assert_eq!(index.bytes, codec::encode(&replayed).unwrap().len());
    assert_eq!(
        replayed.pins[&token].resources,
        BTreeSet::from([
            PinResource::StorageObject("old".into()),
            PinResource::StorageObject("new".into())
        ])
    );
    assert!(apply(&mut state.clone(), &bytes, FRAME).is_err());
    assert!(apply(&mut state.clone(), &bytes, b"CASDLT03").is_err());
    for end in 0..bytes.len() {
        assert!(
            apply(&mut state.clone(), &bytes[..end], EXTENSION_FRAME).is_err(),
            "{end}"
        );
    }
    for invalid in [
        BTreeMap::new(),
        BTreeMap::from([(PinToken::fresh().unwrap(), staging("new"))]),
        BTreeMap::from([(token.clone(), staging("old"))]),
        BTreeMap::from([(
            token.clone(),
            DataPin {
                resources: BTreeSet::new(),
                ..staging("new")
            },
        )]),
        BTreeMap::from([(
            token.clone(),
            DataPin {
                catalog: Some(vec![1]),
                ..staging("new")
            },
        )]),
        BTreeMap::from([(
            token.clone(),
            DataPin {
                scope: PinScope::Snapshot { generation: 1 },
                ..staging("new")
            },
        )]),
    ] {
        assert!(
            apply(
                &mut state.clone(),
                &extension_body(&state, invalid),
                EXTENSION_FRAME
            )
            .is_err()
        );
    }
    let mut metadata = state.clone();
    metadata.pins.get_mut(&token).unwrap().scope = PinScope::Metadata;
    metadata.pins.get_mut(&token).unwrap().resources.clear();
    assert!(
        apply(
            &mut metadata.clone(),
            &extension_body(&metadata, pins.clone()),
            EXTENSION_FRAME
        )
        .is_err()
    );

    let mut next = state.clone();
    next.revision += 1;
    let mut invalid_globals = Changes::new(state.revision).encode(&next).unwrap();
    invalid_globals.extend(
        codec::encode(&PinInventory {
            revision: 1,
            pins: pins.clone(),
            ..Default::default()
        })
        .unwrap(),
    );
    assert!(apply(&mut state.clone(), &invalid_globals, EXTENSION_FRAME).is_err());

    // One token cannot have both a replacement and an extension in one frame.
    next.pins.get_mut(&token).unwrap().catalog = Some(vec![2]);
    let mut changes = Changes::new(state.revision);
    changes.absorb(&Touched::diff(&state, &next));
    let mut conflicting = changes.encode(&next).unwrap();
    conflicting.extend(
        codec::encode(&PinInventory {
            pins,
            ..Default::default()
        })
        .unwrap(),
    );
    assert!(apply(&mut state.clone(), &conflicting, EXTENSION_FRAME).is_err());
}

#[test]
fn grouped_full_records_subsume_earlier_and_later_additions() {
    let token = PinToken::fresh().unwrap();
    for registered in [false, true] {
        let before = PinInventory {
            revision: 1,
            pins: if registered {
                BTreeMap::new()
            } else {
                BTreeMap::from([(token.clone(), staging("old"))])
            },
            ..Default::default()
        };
        let mut state = before.clone();
        let mut changes = Changes::new(before.revision);
        if registered {
            state.pins.insert(token.clone(), staging("old"));
            state.revision += 1;
            changes.absorb(&Touched::diff(&before, &state));
        }
        let bytes = extension_body(&state, BTreeMap::from([(token.clone(), staging("first"))]));
        let (touched, _) = apply(&mut state, &bytes, EXTENSION_FRAME).unwrap();
        changes.absorb(&touched);
        let prior = state.clone();
        state.pins.get_mut(&token).unwrap().catalog = Some(vec![1, 2, 3]);
        state.revision += 1;
        changes.absorb(&Touched::diff(&prior, &state));
        let bytes = extension_body(&state, BTreeMap::from([(token.clone(), staging("last"))]));
        let (touched, _) = apply(&mut state, &bytes, EXTENSION_FRAME).unwrap();
        changes.absorb(&touched);
        assert_eq!(changes.frame(), FRAME);
        let mut replayed = before;
        apply(
            &mut replayed,
            &changes.encode(&state).unwrap(),
            changes.frame(),
        )
        .unwrap();
        assert_eq!(replayed, state);
    }
}

#[tokio::test]
async fn mixed_legacy_and_extension_frames_replay_and_append_across_handles() {
    let dir = tempfile::tempdir().unwrap();
    let store = FilePinStore::new(dir.path().join("pins"));
    let token = store.register(staging("seed")).await.unwrap().unwrap();
    let cursor = store
        .local()
        .unwrap()
        .cache
        .lock()
        .unwrap()
        .snapshot
        .as_ref()
        .unwrap()
        .cursor;
    // Whole-inventory callers still emit the original frame format.
    let mut state = store.inventory().await.unwrap();
    state.revision += 1;
    state
        .pins
        .get_mut(&token)
        .unwrap()
        .resources
        .extend(paths("legacy"));
    store.persist_inventory(&state).unwrap();
    let legacy_end = store
        .local()
        .unwrap()
        .cache
        .lock()
        .unwrap()
        .snapshot
        .as_ref()
        .unwrap()
        .cursor;
    assert!(store.protect(&token, paths("delta")).await.unwrap());
    let bytes = std::fs::read(&store.path).unwrap();
    assert_eq!(&bytes[cursor..cursor + 8], FRAME);
    assert_eq!(&bytes[legacy_end..legacy_end + 8], EXTENSION_FRAME);
    let expected = store.inventory().await.unwrap();
    assert_eq!(fresh(&store).await, expected);
    let other = FilePinStore::new(&store.path);
    assert!(other.protect(&token, paths("foreign")).await.unwrap());
    let expected = other.inventory().await.unwrap();
    assert_eq!(store.inventory().await.unwrap(), expected);
    assert_eq!(fresh(&other).await, expected);
    // Corrupt the new frame's version while preserving its original checksum.
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&store.path)
        .unwrap();
    file.seek(SeekFrom::Start(legacy_end as u64)).unwrap();
    file.write_all(b"CASDLT03").unwrap();
    file.sync_all().unwrap();
    assert!(fresh_result(&store).await.is_err());
}

async fn fresh_result(store: &FilePinStore) -> Result<PinInventory, MetadataError> {
    evict(store);
    store.inventory().await
}

#[tokio::test]
async fn grouped_additions_replacements_and_retirement_preserve_exact_state() {
    let dir = tempfile::tempdir().unwrap();
    let store = FilePinStore::new(dir.path().join("pins"));
    let first = store.register(staging("shared")).await.unwrap().unwrap();
    let second = store.register(staging("shared")).await.unwrap().unwrap();
    for collector_active in [false, true] {
        let collector = if collector_active {
            Some(
                store
                    .begin_collection(store.inventory().await.unwrap().revision, None)
                    .await
                    .unwrap()
                    .unwrap(),
            )
        } else {
            None
        };
        let added = if collector_active {
            "with-collector"
        } else {
            "without-collector"
        };
        let outcomes = store
            .edit_group(&[
                Operation::Protect(first.clone(), paths(added)),
                Operation::Protect(first.clone(), paths(added)),
                Operation::Protect(second.clone(), paths(added)),
                Operation::Register(staging("registered")),
            ])
            .unwrap();
        assert!(outcomes.iter().all(Result::is_ok));
        let registered = match &outcomes[3] {
            Ok(Outcome::Token(Some(token))) => token.clone(),
            _ => panic!("registration failed"),
        };
        // A full replacement after an addition must contain that addition;
        // subsequent additions must be folded into the final replacement.
        store
            .edit_group(&[
                Operation::Protect(registered.clone(), paths("temporary")),
                Operation::Release(registered),
            ])
            .unwrap()
            .into_iter()
            .for_each(|outcome| {
                outcome.unwrap();
            });
        let expected = store.inventory().await.unwrap();
        assert_eq!(fresh(&store).await, expected);
        if let Some(collector) = collector {
            store.finish_collection(&collector).await.unwrap();
        }
    }
    store
        .edit_group(&[
            Operation::Protect(first.clone(), paths("last")),
            Operation::Release(first),
            Operation::Protect(second.clone(), paths("last")),
        ])
        .unwrap()
        .into_iter()
        .for_each(|outcome| {
            outcome.unwrap();
        });
    let expected = store.inventory().await.unwrap();
    assert_eq!(fresh(&store).await, expected);
    assert!(
        store
            .claim_deletions(expected.revision, paths("last"))
            .await
            .unwrap()
            .is_none()
    );
    store.release(&second).await.unwrap();
    let revision = store.inventory().await.unwrap().revision;
    let claim = store
        .claim_deletions(revision, paths("last"))
        .await
        .unwrap()
        .unwrap();
    store.finish_deletions(&claim).await.unwrap();
    assert!(fresh(&store).await.pins.is_empty());
}
