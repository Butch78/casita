//! Small end-to-end example of Gix object traits backed by Casita.
//!
//! The actual compatibility layer lives in `casita::experimental::CasitaGixOdb`; this example is
//! intentionally only a client of that public API.
//!
//! ```console
//! cargo run --example gix_casita_odb --features git
//! ```

use std::error::Error;

use casita::experimental::{CasitaGixOdb, CasitaGixOdbOptions, GitObjectFormat, Repository};
use gix::objs::{Find, FindHeader, Write};

type BoxError = Box<dyn Error + Send + Sync + 'static>;

fn main() -> Result<(), BoxError> {
    let root = tempfile::tempdir()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let repository = runtime.block_on(Repository::local(root.path()))?;
    let options = CasitaGixOdbOptions {
        batch_objects: 64,
        batch_bytes: 4 * 1024 * 1024,
        ..CasitaGixOdbOptions::new(GitObjectFormat::Sha1)
    };
    let odb = CasitaGixOdb::with_options(repository.clone(), options.clone())?;

    let blob_body = b"hello from Casita\n";
    let blob = odb.write_buf(gix::objs::Kind::Blob, blob_body)?;
    let mut tree_body = b"100644 hello.txt\0".to_vec();
    tree_body.extend_from_slice(blob.as_bytes());
    let tree = odb.write_buf(gix::objs::Kind::Tree, &tree_body)?;
    let commit_body = format!(
        "tree {tree}\nauthor Casita <casita@invalid> 1700000000 +0000\ncommitter Casita <casita@invalid> 1700000000 +0000\n\nCasita-backed commit\n"
    );
    let commit = odb.write_buf(gix::objs::Kind::Commit, commit_body.as_bytes())?;

    let mut buffer = Vec::new();
    let staged = odb
        .try_find(&commit, &mut buffer)?
        .expect("a staged write is immediately visible");
    assert_eq!(staged.kind, gix::objs::Kind::Commit);
    odb.flush()?;
    drop(odb);

    let reopened = CasitaGixOdb::with_options(repository, options)?;
    let header = FindHeader::try_header(&reopened, &commit)?
        .expect("the flushed commit is visible after reopening");
    println!("commit {commit}: {} bytes in Casita", header.size);
    Ok(())
}
