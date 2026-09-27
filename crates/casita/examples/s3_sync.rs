//! Upload a local tree to S3, copy it into a fresh local repository, and restore it.
//! Requires only the `s3` feature; imports use the supported application API.

use std::path::PathBuf;

use casita::{Repository, RootName};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [source, cache, bucket, prefix, root, checkout] = args.as_slice() else {
        return Err("usage: s3_sync SOURCE CACHE BUCKET PREFIX ROOT CHECKOUT".into());
    };
    let cache = PathBuf::from(cache);
    let name = RootName::try_from(root.as_str())?;
    let local = Repository::local(cache.join("source")).await?;
    let key = local
        .import(casita::import::FilesystemImport::new(source, name.clone()))
        .await?;

    let remote = Repository::s3(bucket, prefix, format!("s3-sync-{}", std::process::id())).await?;
    remote
        .import(casita::import::CopyImport::new(
            &local,
            name.clone(),
            name.clone(),
        ))
        .await?;
    remote.flush().await?;

    let mirror = Repository::local(cache.join("mirror")).await?;
    let copied = mirror
        .import(casita::import::CopyImport::new(
            &remote,
            name.clone(),
            name.clone(),
        ))
        .await?;
    if copied != key {
        return Err("the remote root changed during the copy".into());
    }
    mirror.checkout(&copied, checkout).await?;
    remote.flush().await?;
    mirror.flush().await?;
    local.flush().await?;
    println!("Restored {copied} to {checkout}");
    Ok(())
}
