#![cfg(feature = "cli")]

use casita::experimental::{DataPin, MetadataStore, PinResource, PinScope, TursoMetadataStore};
use std::{collections::BTreeSet, process::Command};

#[tokio::test]
async fn cli_inspects_local_pins_through_an_interrupted_prune_fence() {
    let directory = tempfile::tempdir().unwrap();
    let repository = directory.path().join("repository");
    std::fs::create_dir(&repository).unwrap();
    let state = TursoMetadataStore::open(repository.join("casita.sqlite"))
        .await
        .unwrap();
    let ledger = state.pin_store().await.unwrap();
    let pin = ledger
        .register(DataPin {
            scope: PinScope::Snapshot { generation: 0 },
            catalog: None,
            resources: BTreeSet::from([PinResource::StorageObject("held\npath".into())]),
        })
        .await
        .unwrap()
        .unwrap();
    let collector = ledger
        .begin_collection(ledger.inventory().await.unwrap().revision, None)
        .await
        .unwrap()
        .unwrap();
    let fence = ledger
        .begin_prune(ledger.inventory().await.unwrap().revision)
        .await
        .unwrap()
        .unwrap();
    drop(state);
    let before = ledger.inventory().await.unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_casita"))
        .arg("holds")
        .arg(&repository)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(output["collectors"], serde_json::json!([]));
    assert_eq!(output["coordination"], serde_json::Value::Null);
    assert_eq!(output["state"]["collector"], collector.to_string());
    assert_eq!(output["state"]["logical_prune"], fence.to_string());
    assert_eq!(output["state"]["pins"][0]["token"], pin.to_string());
    assert_eq!(
        output["state"]["pins"][0]["scope"],
        serde_json::json!({"kind": "snapshot", "generation": 0})
    );
    assert_eq!(
        output["state"]["pins"][0]["resources"][0]["path"],
        "held\npath"
    );
    assert_eq!(
        ledger.inventory().await.unwrap(),
        before,
        "diagnostics must not mutate the data ledger"
    );
    let missing = directory.path().join("missing");
    let output = Command::new(env!("CARGO_BIN_EXE_casita"))
        .arg("holds")
        .arg(&missing)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!missing.exists());
}
