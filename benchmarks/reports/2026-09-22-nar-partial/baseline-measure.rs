async fn measure(
    reader: &RetainedReader,
    root: &Node,
    request: &NarRequirements,
    scrub: bool,
) -> Result<VerifiedNarReport, NarError> {
    let start = Instant::now();
    request.validate(Some(root))?;
    let store = reader.hold.repository().nar_store.as_ref();
    let generation = match store {
        Some(store) => store.generation().await.map_err(NarError::storage)?,
        None => 0,
    };
    let key = identity(root);
    let flight = store.map(|s| s.flight(&key));
    let _guard = match &flight {
        Some(lock) => Some(lock.lock().await),
        None => None,
    };
    available(reader, root).await?;
    let old = match store {
        Some(store) => store.get(&key).await?,
        None => None,
    };
    if !scrub
        && let Some(facts) = &old
        && facts.complete(request)
    {
        return report(
            reader.clone(),
            root.clone(),
            facts.clone(),
            request,
            NarVerificationStats {
                association_hit: true,
                verification_time: start.elapsed(),
                ..Default::default()
            },
        );
    }
    let mut missing = request.clone();
    if !scrub && let Some(old) = &old {
        missing
            .hashes
            .retain(|(m, a)| !old.values.contains_key(&vec![*m as u8, *a as u8]));
        if request
            .keys()
            .iter()
            .filter(|k| k[0] == 4)
            .all(|k| old.values.contains_key(k))
        {
            missing.needles.clear();
        }
    }
    let measured = stream::measure_tree(reader, root, &missing, old.as_ref(), scrub).await;
    let (mut facts, mut stats) = match measured {
        Ok(result) => result,
        Err(error) => {
            if let Some(store) = store {
                // Verified opens and reads already invalidate on damage; this
                // covers tree decoding. Busy or throttled storage is not damage.
                let damaged = match &error {
                    NarError::Io(error) => crate::blob::is_damaged_payload_io_error(error),
                    NarError::Storage(error) => matches!(
                        error.kind(),
                        crate::ErrorKind::Corrupt | crate::ErrorKind::InvalidData
                    ),
                    NarError::Invalid(_) | NarError::Conflict => false,
                };
                if matches!(error, NarError::Conflict) {
                    store.quarantine(&key).await?;
                } else if damaged {
                    store.record_read_failure().await;
                }
            }
            return Err(error);
        }
    };
    if let Some(store) = store {
        // Measured from verified reads. An invalidation that landed since
        // withholds the association, not the measurement.
        if let Some(merged) = store.merge(&key, &facts, generation).await? {
            facts = merged;
        }
    }
    stats.verification_time = start.elapsed();
    report(reader.clone(), root.clone(), facts, request, stats)
}
