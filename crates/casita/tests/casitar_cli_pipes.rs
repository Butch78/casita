#![cfg(feature = "experimental")]

//! Black-box coverage for portable Casitar exchange over standard I/O.

#![cfg(feature = "cli")]

use std::process::{Command, Output, Stdio};

use casita::experimental::{ClosureStatus, MetadataStore as _, Repository, RootName};

fn casita() -> Command {
    Command::new(env!("CARGO_BIN_EXE_casita"))
}

fn assert_success(context: &str, output: Output) {
    assert!(
        output.status.success(),
        "{context} failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

fn pipe(mut producer: Command, mut consumer: Command, context: &str) -> Output {
    producer.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut producer = producer.spawn().expect("start archive producer");
    consumer.stdin(Stdio::from(
        producer
            .stdout
            .take()
            .expect("producer stdout was configured as a pipe"),
    ));
    let consumer_output = consumer.output().expect("run archive consumer");
    let producer_output = producer
        .wait_with_output()
        .expect("wait for archive producer");
    assert_success(&format!("{context} producer"), producer_output);
    assert_success(&format!("{context} consumer"), consumer_output.clone());
    consumer_output
}

#[tokio::test]
async fn two_repositories_exchange_every_multi_root_archive_over_pipes() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("source");
    let destination = temporary.path().join("destination");
    let selected_but_unused = temporary.path().join("selected-but-unused");
    let first = temporary.path().join("first");
    let second = temporary.path().join("second");
    std::fs::create_dir_all(first.join("nested")).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    std::fs::write(first.join("nested/one.txt"), b"first portable root").unwrap();
    std::fs::write(second.join("two.txt"), b"second portable root").unwrap();

    let mut import_first = casita();
    import_first
        .arg("--repository")
        .arg(&source)
        .args(["import"])
        .arg(&first)
        .args(["--root", "releases/first"]);
    assert_success("import first source root", import_first.output().unwrap());

    let mut import_second = casita();
    import_second
        .arg("--repository")
        .arg(&source)
        .args(["import"])
        .arg(&second)
        .args(["--root", "releases/second"]);
    assert_success("import second source root", import_second.output().unwrap());

    let mut verify_producer = casita();
    verify_producer.arg("--repository").arg(&source).args([
        "archive",
        "create",
        "--root",
        "releases/first",
        "--root",
        "releases/second",
        "--output",
        "-",
    ]);
    let mut verifier = casita();
    verifier
        .arg("--repository")
        .arg(&selected_but_unused)
        .args(["archive", "verify", "-"]);
    pipe(verify_producer, verifier, "pipe verification");
    assert!(
        !selected_but_unused.exists(),
        "verify must not open or mutate its selected durable repository"
    );

    let mut import_producer = casita();
    import_producer.arg("--repository").arg(&source).args([
        "archive",
        "create",
        "--root",
        "releases/first",
        "--root",
        "releases/second",
        "--output",
        "-",
    ]);
    let mut importer = casita();
    importer.arg("--repository").arg(&destination).args([
        "archive",
        "import",
        "-",
        "--root-prefix",
        "exchange/release",
    ]);
    let imported = pipe(import_producer, importer, "pipe import");

    let source_repository = Repository::local(&source).await.unwrap();
    let source_snapshot = source_repository.metadata().snapshot().await.unwrap();
    let mut expected_roots = [
        source_snapshot
            .root(&RootName::try_from("releases/first").unwrap())
            .await
            .unwrap()
            .unwrap(),
        source_snapshot
            .root(&RootName::try_from("releases/second").unwrap())
            .await
            .unwrap()
            .unwrap(),
    ];
    expected_roots.sort();

    let destination_repository = Repository::local(&destination).await.unwrap();
    let destination_snapshot = destination_repository.metadata().snapshot().await.unwrap();
    assert!(
        String::from_utf8_lossy(&imported.stdout).contains(&format!(
            "destination-revision {}",
            destination_snapshot.revision()
        )),
        "import must report the one revision that published all roots"
    );
    for (index, expected) in expected_roots.iter().enumerate() {
        let name = RootName::try_from(format!("exchange/release/{index}")).unwrap();
        assert_eq!(
            destination_snapshot.root(&name).await.unwrap(),
            Some(expected.clone()),
            "destination retained canonical archive root {index}",
        );
        assert!(matches!(
            destination_repository
                .verify_closure(expected)
                .await
                .unwrap(),
            ClosureStatus::Complete { .. }
        ));
    }
}

#[test]
fn verification_drains_temporary_repository_cleanup_across_batches() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("source");
    let tree = temporary.path().join("tree");
    let archive = temporary.path().join("fixture.casitar");
    std::fs::create_dir(&tree).unwrap();
    for index in 0..256u32 {
        std::fs::write(tree.join(format!("file-{index}")), index.to_le_bytes()).unwrap();
    }
    assert_success(
        "seed",
        casita()
            .arg("--repository")
            .arg(&source)
            .arg("import")
            .arg(&tree)
            .args(["--root", "fixture"])
            .output()
            .unwrap(),
    );
    assert_success(
        "export",
        casita()
            .arg("--repository")
            .arg(&source)
            .args(["archive", "create", "--root", "fixture", "--output"])
            .arg(&archive)
            .output()
            .unwrap(),
    );
    for _ in 0..3 {
        let verified = casita()
            .args(["archive", "verify"])
            .arg(&archive)
            .arg("--json")
            .output()
            .unwrap();
        assert_success("verify and drain", verified.clone());
        let report: serde_json::Value = serde_json::from_slice(&verified.stdout).unwrap();
        assert_eq!(report["validity"], "verified");
        assert_eq!(report["stats"]["records"], 257);
        assert!(!String::from_utf8_lossy(&verified.stderr).contains("background task failed"));
    }
    // Failure after ingesting frames must also drain before deleting the store.
    let mut bytes = std::fs::read(&archive).unwrap();
    bytes.pop();
    std::fs::write(&archive, bytes).unwrap();
    let failed = casita()
        .args(["archive", "verify"])
        .arg(&archive)
        .arg("--json")
        .output()
        .unwrap();
    assert!(!failed.status.success());
    assert!(!String::from_utf8_lossy(&failed.stderr).contains("background task failed"));
}
