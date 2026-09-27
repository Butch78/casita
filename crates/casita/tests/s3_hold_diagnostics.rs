#![cfg(all(feature = "cli", feature = "s3"))]

#[path = "support/rustfs.rs"]
mod rustfs;

use casita::experimental::{DataPin, MetadataStore, PinResource, PinScope, Wal3MetadataStore};
use std::collections::BTreeSet;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn abandoned_hold_owner() {
    let Some(token_file) = std::env::var_os("CASITA_HOLD_TOKEN_FILE") else {
        return;
    };
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let state = Wal3MetadataStore::open_s3(
            "casita-application-test",
            "diagnostics/state",
            "diagnostic\nworker",
        )
        .await
        .unwrap();
        let hold = state.try_collection_lease().await.unwrap().unwrap();
        let token = state.repository_holds().await.unwrap().pop().unwrap().token;
        let ledger = state.pin_store().await.unwrap();
        let pin = ledger
            .register(DataPin {
                scope: PinScope::Staging,
                catalog: None,
                resources: BTreeSet::from([PinResource::MetadataObject("held-metadata".into())]),
            })
            .await
            .unwrap()
            .unwrap();
        let collector = ledger
            .begin_collection(ledger.inventory().await.unwrap().revision, None)
            .await
            .unwrap()
            .unwrap();
        let deletion = ledger
            .claim_deletions(
                ledger.inventory().await.unwrap().revision,
                BTreeSet::from([PinResource::MetadataObject("claimed-metadata".into())]),
            )
            .await
            .unwrap()
            .unwrap();
        std::fs::write(
            token_file,
            serde_json::json!({
                "operational": token.as_str(), "pin": pin.to_string(),
                "collector": collector.to_string(), "deletion": deletion.to_string(),
            })
            .to_string(),
        )
        .unwrap();
        // This process exits without releasing its hold, modeling a dead owner.
        std::mem::forget(hold);
    });
}

fn finish(
    mut command: Command,
    work: &std::path::Path,
    name: &str,
) -> (std::process::ExitStatus, String, String) {
    let stdout = work.join(format!("{name}.stdout"));
    let stderr = work.join(format!("{name}.stderr"));
    command.stdout(Stdio::from(std::fs::File::create(&stdout).unwrap()));
    command.stderr(Stdio::from(std::fs::File::create(&stderr).unwrap()));
    let mut child = command.spawn().unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return (
                status,
                std::fs::read_to_string(stdout).unwrap(),
                std::fs::read_to_string(stderr).unwrap(),
            );
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!(
                "{name} timed out: {}",
                std::fs::read_to_string(stderr).unwrap()
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn cli_inspects_collector_ownership_without_blocking_read_open() {
    let fixture = rustfs::Rustfs::start();
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(fixture.create_bucket());
    let work = tempfile::tempdir().unwrap();
    let token_file = work.path().join("token");
    let mut owner = Command::new(std::env::current_exe().unwrap());
    owner
        .args(["abandoned_hold_owner", "--exact", "--nocapture"])
        .env("CASITA_HOLD_TOKEN_FILE", &token_file);
    fixture.configure(&mut owner);
    let (status, _, stderr) = finish(owner, work.path(), "owner");
    assert!(status.success(), "{stderr}");
    let tokens: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(token_file).unwrap()).unwrap();
    let unused_local = work.path().join("must-not-open");
    for attempt in 0..2 {
        let mut inspect = Command::new(env!("CARGO_BIN_EXE_casita"));
        inspect.arg("--repository").arg(&unused_local).args([
            "holds",
            "s3://casita-application-test/diagnostics",
            "--json",
        ]);
        fixture.configure(&mut inspect);
        let (status, stdout, stderr) = finish(inspect, work.path(), &format!("inspect-{attempt}"));
        assert!(status.success(), "{stderr}");
        let output: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(
            output["collectors"],
            serde_json::json!([
                {"token": tokens["operational"], "writer": "diagnostic\nworker", "exclusive": true}
            ])
        );
        assert_eq!(output["state"]["collector"], tokens["collector"]);
        assert_eq!(output["state"]["logical_prune"], serde_json::Value::Null);
        assert_eq!(
            output["state"]["pins"],
            serde_json::json!([{
                "token": tokens["pin"], "scope": {"kind": "staging"}, "released": false,
                "catalog": null, "resources": [{"kind": "metadata_object", "path": "held-metadata"}],
            }])
        );
        assert_eq!(
            output["state"]["deletions"],
            serde_json::json!([{
                "token": tokens["deletion"], "resources": [{"kind": "metadata_object", "path": "claimed-metadata"}],
            }])
        );
        assert!(output["coordination"]["revision"].is_u64());
    }
    assert!(!unused_local.exists());
    let mut read = Command::new(env!("CARGO_BIN_EXE_casita"));
    read.args([
        "sync",
        "--from",
        "s3://casita-application-test/diagnostics",
        "--to",
    ])
    .arg(work.path().join("destination"))
    .args(["--root", "test"])
    .env_remove("RUST_LOG");
    fixture.configure(&mut read);
    let (status, _, stderr) = finish(read, work.path(), "reader");
    assert!(!status.success());
    assert!(stderr.contains("root `test`"), "{stderr}");
    assert!(stderr.contains("absent"), "{stderr}");
    assert!(!stderr.contains("repository admission blocked"), "{stderr}");
}
