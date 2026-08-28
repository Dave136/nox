mod model;
mod store;

#[allow(unused_imports)]
pub(crate) use model::{VaultEntry, VaultRegistry, slug_for};
#[allow(unused_imports)]
pub(crate) use store::RegistryLoad;
#[allow(unused_imports)]
pub(crate) use store::{adopt_legacy_vault, load_registry, registry_path, save_registry};
