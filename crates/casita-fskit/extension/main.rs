#![no_main]
// Apple's extension entry point is selected by build.rs. No Swift or bridge process.
use fskit_native::{
    native::{Extension, Resource},
    Filesystem,
};

fn open(resource: Resource<'_>) -> std::io::Result<Box<dyn Filesystem>> {
    #[cfg(feature = "repository")]
    {
        let Resource::Path(path) = resource else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "expected repository path",
            ));
        };
        let backend = if cfg!(feature = "production") {
            casita_fskit::repository::Backend::open_native(path)
        } else {
            casita_fskit::repository::Backend::open(path)
        }
        .map_err(std::io::Error::other)?;
        Ok(Box::new(backend))
    }
    #[cfg(not(feature = "repository"))]
    {
        let _ = resource;
        Ok(Box::new(casita_fskit::MemoryFilesystem::default()))
    }
}

#[ctor::ctor(unsafe)]
fn setup() {
    fskit_native::native::register(Extension {
        name: if cfg!(feature = "repository") {
            "Casita repository"
        } else {
            "Casita memory fixture"
        },
        filesystem_type: if cfg!(feature = "production") {
            "casita"
        } else if cfg!(feature = "repository") {
            "casitarepo"
        } else {
            "casitanative"
        },
        path_resources: cfg!(feature = "repository"),
        block_resources: !cfg!(feature = "repository"),
        open,
    })
    .expect("register native FSKit extension");
}
