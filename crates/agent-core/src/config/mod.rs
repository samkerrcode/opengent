mod data_root;
mod product;

pub(crate) use data_root::write_private;
pub use data_root::{resolve_data_root, DataRoot};
pub use product::{product, ProductInfo};
