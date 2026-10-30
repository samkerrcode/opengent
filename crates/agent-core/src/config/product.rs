use serde::Serialize;
use ts_rs::TS;

/// Product identity, sourced from the repository-level `product.json` at build time.
///
/// `display_name` is presentation-only and may change. `slug`, `data_dir_name` and
/// `identifier` are stable technical identities used for paths, keychain namespaces and
/// bundle IDs; changing them requires an explicit migration.
#[derive(Debug, Clone, Copy, Serialize, TS)]
#[ts(export)]
pub struct ProductInfo {
    pub display_name: &'static str,
    pub slug: &'static str,
    pub data_dir_name: &'static str,
    pub identifier: &'static str,
    pub version: &'static str,
    pub description: &'static str,
}

const PRODUCT: ProductInfo = ProductInfo {
    display_name: env!("PRODUCT_DISPLAY_NAME"),
    slug: env!("PRODUCT_SLUG"),
    data_dir_name: env!("PRODUCT_DATA_DIR_NAME"),
    identifier: env!("PRODUCT_IDENTIFIER"),
    version: env!("PRODUCT_VERSION"),
    description: env!("PRODUCT_DESCRIPTION"),
};

pub const fn product() -> &'static ProductInfo {
    &PRODUCT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_fields_are_populated() {
        let p = product();
        assert!(!p.display_name.is_empty());
        assert!(p.data_dir_name.starts_with('.'));
        assert!(p.identifier.contains('.'));
        assert_eq!(p.version.split('.').count(), 3);
    }
}
