mod data_root;
mod product;

pub(crate) use data_root::write_private;
pub use data_root::{data_dir_env_var, log_env_var, resolve_data_root, DataRoot};
pub use product::{product, ProductInfo};
