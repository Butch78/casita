---
title: Define a Custom Object Format
description: Verify a new object's identity, payload, and links in every repository that uses it.
---

Use a custom `ObjectFormat` when an object needs its own key namespace or
format rules that another repository must enforce. An importer can parse a
source, but the format verifier decides whether its stored object is valid.
The destination must register the same verifier before it can accept those
objects through sync.

This API is in `casita::experimental` and requires the `experimental` Cargo
feature. For ordinary blobs and filesystem trees, start with the
[supported library API](/library/) and [importers](../adding-an-importer/).

## Verify an object

An `ObjectFormat` owns one namespace. Its `verify` method checks the native ID,
reads the full payload within a bound, validates its encoding, and returns the
exact direct links. The repository supplies the read context and creates a
sealed `VerifiedObject`; a caller cannot publish an unchecked record in its
place.

This example stores newline-terminated UTF-8 documents. Each key uses the
BLAKE3 digest of the complete document as its native ID. Documents have no
links. The example registers the format, stages one document, and names it:

```rust
# async fn example() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
use std::sync::Arc;

use casita::experimental::{
    async_trait, BlobFormat, Digest, DirectoryFormat, FormatError, FormatLimits,
    FormatRegistry, MemoryBlobStore, MemoryMetadataStore, NamespaceId, ObjectFormat,
    ObjectKey, Repository, RootName, VerificationContext, VerifiedObject,
};

struct DocumentFormat {
    namespace: NamespaceId,
}

#[async_trait]
impl ObjectFormat for DocumentFormat {
    fn namespace(&self) -> &NamespaceId {
        &self.namespace
    }

    async fn verify(
        &self,
        mut context: VerificationContext<'_>,
        limits: &FormatLimits,
    ) -> Result<VerifiedObject, FormatError> {
        let expected = context.key().native_digest().ok_or_else(|| {
            FormatError::NativeIdLength {
                namespace: self.namespace.clone(),
                actual: context.key().native_id().len(),
            }
        })?;

        let bytes = context
            .read_to_end_bounded(
                (1024 * 1024).min(limits.max_metadata_bytes).min(limits.max_payload_bytes)
            )
            .await?;
        let text = std::str::from_utf8(&bytes).map_err(|error| {
            FormatError::InvalidPayload {
                namespace: self.namespace.clone(),
                message: error.to_string(),
            }
        })?;
        if !text.ends_with('\n') {
            return Err(FormatError::InvalidPayload {
                namespace: self.namespace.clone(),
                message: "document must end with a newline".into(),
            });
        }

        let actual = context.observed_digest();
        if actual != expected {
            return Err(FormatError::NativeIdentityMismatch {
                key: context.key().clone(),
                expected,
                actual,
            });
        }
        context.finish(Vec::new())
    }
}

let namespace: NamespaceId = "example.document.v1".parse()?;
let formats = FormatRegistry::new([
    Arc::new(BlobFormat::default()) as Arc<dyn ObjectFormat>,
    Arc::new(DirectoryFormat::default()) as Arc<dyn ObjectFormat>,
    Arc::new(DocumentFormat { namespace: namespace.clone() }) as Arc<dyn ObjectFormat>,
])?;
let repository = Repository::with_formats(
    MemoryBlobStore::new(),
    MemoryMetadataStore::new()?,
    formats,
    FormatLimits::default(),
);

let bytes = b"hello\n";
let key = ObjectKey::new(namespace, Digest::hash(bytes).as_bytes().to_vec())?;
let mutation = repository.mutation_session().await?;
let staged = mutation.stage_object(key.clone(), bytes).await?;
mutation
    .publish_rooted(vec![staged], RootName::try_from("documents/hello")?, key)
    .await?;
# Ok(())
# }
```

`read_to_end_bounded` consumes the whole payload or fails at the configured
limit. `context.finish` requires end of input, records the payload digest and
size, and seals the declared links. In this example, an empty link list is part
of the format's meaning.

## Register every format you need

`FormatRegistry::new` contains exactly the formats you list and rejects two
verifiers for the same namespace. The example includes raw blobs, canonical
directories, and documents. Add any other formats your repository must read or
publish. The registry is fixed when the repository opens, so repositories that
exchange custom objects must each register the matching verifier.

For a format with child objects, return their canonical direct keys from
`context.finish(links)`. Those links control closure checks, roots, sync, and
collection. Override `verify_links` only when validity also depends on a
declared direct child's verified record or payload. Read it through
`DirectLinkView`; keep verification deterministic and independent of the
network, clock, and mutable roots.

## Test the format

Test valid input, malformed or truncated payloads, wrong native IDs, size and
link limits, link order, and relations to missing or incompatible children.
Check that publication rejects an invalid object and that a receiving
repository with the same registry verifies it after sync.

See [Object Formats](../../reference/object-formats/) for built-in namespaces
and the [Experimental Rust API](../../reference/experimental-rust-api/#object-formats)
for the verifier contract.
