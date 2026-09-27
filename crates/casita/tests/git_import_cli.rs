#![cfg(all(feature = "cli", feature = "git"))]

use std::path::Path;
use std::process::{Command, Output};

use casita::experimental::GitViewBody;
use tokio::io::AsyncReadExt;

fn success(output: Output) -> Output {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

async fn read_view(path: &Path) -> (casita::ObjectKey, GitViewBody) {
    let repository = casita::Repository::local(path).await.unwrap();
    let key = repository
        .root(&"git/upstream".try_into().unwrap())
        .await
        .unwrap()
        .unwrap();
    let mut reader = repository.open(&key).await.unwrap().unwrap();
    let mut payload = Vec::new();
    reader.read_to_end(&mut payload).await.unwrap();
    drop(reader);
    repository.flush().await.unwrap();
    (key, GitViewBody::decode(&payload).unwrap())
}

#[tokio::test]
async fn git_cli_preserves_selection_cache_options_and_root_on_invalid_refs() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let destination = temp.path().join("repository");
    std::fs::create_dir(&source).unwrap();
    let git = |args: &[&str]| {
        success(
            Command::new("git")
                .current_dir(&source)
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", temp.path().join("empty-config"))
                .args(args)
                .output()
                .unwrap(),
        );
    };
    git(&["init", "--initial-branch=main"]);
    std::fs::write(source.join("hello"), b"CLI Git import\n").unwrap();
    git(&["add", "hello"]);
    git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-m",
        "fixture",
    ]);
    git(&["branch", "other"]);
    git(&["tag", "v1"]);
    git(&["repack", "-ad"]);

    let import = |options: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_casita"))
            .current_dir(temp.path())
            .env("RUST_LOG", "off")
            .arg("--repository")
            .arg(&destination)
            .args(["import", "-i", "git"])
            .arg(&source)
            .args(["--git-view", "upstream"])
            .args(options)
            .output()
            .unwrap()
    };

    success(import(&[]));
    let (_, full) = read_view(&destination).await;
    assert_eq!(
        full.refs
            .keys()
            .map(|name| name.as_str())
            .collect::<Vec<_>>(),
        ["refs/heads/main", "refs/heads/other", "refs/tags/v1"]
    );
    assert!(full.pack.is_some(), "fixture must exercise pack caching");

    success(import(&[
        "--git-ref",
        "refs/heads/main",
        "--git-max-cached-pack-bytes",
        "0",
        "--git-concurrency",
        "2",
        "--git-max-buffered-bytes",
        "65536",
    ]));
    let (selected_key, selected) = read_view(&destination).await;
    assert_eq!(
        selected
            .refs
            .keys()
            .map(|name| name.as_str())
            .collect::<Vec<_>>(),
        ["refs/heads/main"]
    );
    // All fixture refs target the same commit, so this exact pack would still
    // qualify for retention if the zero limit were lost by CLI dispatch.
    assert_eq!(selected.objects, full.objects);
    assert!(selected.pack.is_none());

    let invalid = import(&["--git-ref", "invalid ref"]);
    assert_eq!(invalid.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&invalid.stderr);
    assert!(stderr.contains("error[invalid_input]"), "{stderr}");
    assert!(invalid.stdout.is_empty());
    let (after_key, after) = read_view(&destination).await;
    assert_eq!(after_key, selected_key);
    assert_eq!(after, selected);
}
