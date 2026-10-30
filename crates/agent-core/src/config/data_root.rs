use std::path::{Path, PathBuf};

use crate::{config::product, Error, Result};

/// The application data root (default `~/<data_dir_name>`), created with private permissions.
#[derive(Debug, Clone)]
pub struct DataRoot {
    path: PathBuf,
}

impl DataRoot {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn attachments_dir(&self) -> PathBuf {
        self.path.join("attachments")
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.path.join("cache")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.path.join("logs")
    }

    pub fn database_path(&self) -> PathBuf {
        self.path.join("data.sqlite3")
    }
}

/// Name of the environment variable that overrides the data root, derived from the product slug
/// (e.g. `OPENGENT_DATA_DIR`). Intended for portable installs and tests.
pub fn data_dir_env_var() -> String {
    format!(
        "{}_DATA_DIR",
        product().slug.to_ascii_uppercase().replace('-', "_")
    )
}

/// Resolves and creates the data root. `override_path` wins, then the env var, then home.
pub fn resolve_data_root(override_path: Option<&Path>) -> Result<DataRoot> {
    let path = match override_path {
        Some(p) => p.to_path_buf(),
        None => match std::env::var_os(data_dir_env_var()) {
            Some(p) if !p.is_empty() => PathBuf::from(p),
            _ => dirs::home_dir()
                .ok_or_else(|| Error::Io("could not determine the user home directory".into()))?
                .join(product().data_dir_name),
        },
    };
    create_private_dir(&path)?;
    for sub in ["attachments", "cache", "logs"] {
        create_private_dir(&path.join(sub))?;
    }
    Ok(DataRoot { path })
}

fn create_private_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn override_path_is_used_and_subdirs_created() {
        let tmp =
            std::env::temp_dir().join(format!("{}-test-{}", product().slug, uuid::Uuid::new_v4()));
        let root = resolve_data_root(Some(&tmp)).unwrap();
        assert_eq!(root.path(), tmp);
        assert!(root.attachments_dir().is_dir());
        assert!(root.cache_dir().is_dir());
        assert!(root.logs_dir().is_dir());
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn env_var_name_derives_from_slug() {
        assert_eq!(
            data_dir_env_var(),
            format!("{}_DATA_DIR", product().slug.to_ascii_uppercase())
        );
    }
}
