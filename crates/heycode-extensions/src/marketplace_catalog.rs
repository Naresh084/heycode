//! Whole-generation admission of one pinned marketplace catalog document.

use std::collections::BTreeSet;

use serde::Deserialize;

use crate::marketplace_error::{invalid_entry, invalid_field};
use crate::wire::WireSignature;
use crate::{
    CatalogDigest, CatalogEntry, MARKETPLACE_CATALOG_SCHEMA_VERSION, MarketplaceCatalog,
    MarketplaceError, MarketplaceSource, PackageContentHash, PluginId, PluginSourceKind,
    PluginVersion, SignatureState,
};

const MAX_CATALOG_BYTES: usize = 512 * 1024;
const MAX_CATALOG_ENTRIES: usize = 1_024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireCatalog {
    schema_version: u32,
    marketplace: String,
    packages: Vec<WirePackage>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WirePackage {
    id: String,
    version: String,
    source_kind: PluginSourceKind,
    locator: String,
    content: String,
    signature: Option<WireSignature>,
}

impl MarketplaceCatalog {
    /// Verify the pinned catalog digest over exact fetched bytes, then admit
    /// the complete generation.
    ///
    /// Order is load-bearing. The byte budget is applied first, the pinned
    /// digest second, and the document is decoded only after both hold: an
    /// unpinned or oversized catalog is never parsed, so no attacker-authored
    /// row reaches the validator at all.
    ///
    /// # Errors
    /// Oversized bytes, a digest that differs from the source pin, non-UTF-8
    /// bytes, malformed or unknown TOML fields, an unsupported schema, too
    /// many rows, or any invalid row. One bad row rejects the whole
    /// generation.
    pub fn parse_pinned(source: &MarketplaceSource, raw: &[u8]) -> Result<Self, MarketplaceError> {
        if raw.len() > MAX_CATALOG_BYTES {
            return Err(MarketplaceError::CatalogTooLarge {
                limit: MAX_CATALOG_BYTES,
            });
        }
        let digest = CatalogDigest::of_bytes(raw);
        if &digest != source.catalog_digest() {
            return Err(MarketplaceError::CatalogDigestMismatch {
                pinned: source.catalog_digest().clone(),
                found: digest,
            });
        }
        let text = std::str::from_utf8(raw).map_err(|_| MarketplaceError::CatalogNotUtf8)?;
        let wire =
            toml::from_str::<WireCatalog>(text).map_err(|_| MarketplaceError::InvalidDocument)?;
        if wire.schema_version != MARKETPLACE_CATALOG_SCHEMA_VERSION {
            return Err(MarketplaceError::UnsupportedCatalogSchema {
                found: wire.schema_version,
                supported: MARKETPLACE_CATALOG_SCHEMA_VERSION,
            });
        }
        let marketplace = crate::MarketplaceId::new(wire.marketplace)?;
        if &marketplace != source.id() {
            return Err(invalid_field(
                "marketplace",
                "catalog identity differs from the configured marketplace source",
            ));
        }
        if wire.packages.len() > MAX_CATALOG_ENTRIES {
            return Err(MarketplaceError::TooManyEntries {
                limit: MAX_CATALOG_ENTRIES,
            });
        }
        let mut entries = Vec::with_capacity(wire.packages.len());
        let mut seen = BTreeSet::new();
        for (index, package) in wire.packages.into_iter().enumerate() {
            let entry = validate_entry(index, package, source, &marketplace)?;
            if !seen.insert((entry.id().clone(), entry.version().as_str().to_owned())) {
                return Err(invalid_entry(
                    index,
                    "packages.version",
                    "the same id and version is offered by an earlier row",
                ));
            }
            entries.push(entry);
        }
        entries.sort_by(|left, right| {
            left.id().cmp(right.id()).then_with(|| {
                left.version()
                    .precedence_cmp(right.version())
                    .then_with(|| left.version().as_str().cmp(right.version().as_str()))
            })
        });
        Ok(Self {
            schema_version: MARKETPLACE_CATALOG_SCHEMA_VERSION,
            marketplace,
            digest,
            entries,
        })
    }
}

fn validate_entry(
    index: usize,
    package: WirePackage,
    source: &MarketplaceSource,
    marketplace: &crate::MarketplaceId,
) -> Result<CatalogEntry, MarketplaceError> {
    let id = PluginId::new(package.id).map_err(|_| {
        invalid_entry(
            index,
            "packages.id",
            "must be marketplace/plugin with lowercase kebab-case segments",
        )
    })?;
    if id.as_str().split('/').next() != Some(marketplace.as_str()) {
        return Err(invalid_entry(
            index,
            "packages.id",
            "namespace must be the catalog's own marketplace identity",
        ));
    }
    let version = PluginVersion::parse(package.version).map_err(|_| {
        invalid_entry(
            index,
            "packages.version",
            "must be a valid Semantic Versioning 2.0 value",
        )
    })?;
    if source.kind().is_remote() && package.source_kind == PluginSourceKind::Local {
        return Err(invalid_entry(
            index,
            "packages.source_kind",
            "a remote catalog may not vend a local package source",
        ));
    }
    crate::validator::validate_source_locator(package.source_kind, &package.locator).map_err(
        |_| {
            invalid_entry(
                index,
                "packages.locator",
                "must be a bounded public locator without credentials, query strings, or traversal",
            )
        },
    )?;
    let content = PackageContentHash::parse(&package.content).map_err(|_| {
        invalid_entry(
            index,
            "packages.content",
            "must be sha256 followed by 64 lowercase hexadecimal digits",
        )
    })?;
    let signature = match package.signature {
        None => SignatureState::Absent,
        Some(wire) => {
            SignatureState::Present(crate::validator::validate_signature(wire).map_err(|_| {
                invalid_entry(
                    index,
                    "packages.signature",
                    "must be a stable key reference and canonical Ed25519 base64",
                )
            })?)
        }
    };
    Ok(CatalogEntry::new(
        id,
        version,
        package.source_kind,
        package.locator,
        content,
        signature,
    ))
}
