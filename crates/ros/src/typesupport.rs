//! Finds the type support of any installed message type at runtime, as `ros2 bag` does: every message package
//! installs `lib<pkg>__rosidl_typesupport_c.so` with a function returning the handle for each of its types.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, anyhow};
use archon_msg::TypeName;
use libloading::Library;
use r2r_rcl::rosidl_message_type_support_t;

#[derive(Default)]
pub struct TypeSupports {
    /// Kept loaded, since the handles point into them
    libraries: HashMap<String, Library>,
    handles: HashMap<String, *const rosidl_message_type_support_t>,
}

impl TypeSupports {
    pub fn get(&mut self, ty: &str) -> anyhow::Result<*const rosidl_message_type_support_t> {
        if let Some(&handle) = self.handles.get(ty) {
            return Ok(handle);
        }
        let name = TypeName::parse(ty).ok_or_else(|| anyhow!("{ty:?} isn't a message type"))?;
        let library = match self.libraries.entry(name.package.clone()) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => entry.insert(open(&name.package)?),
        };
        let symbol =
            format!("rosidl_typesupport_c__get_message_type_support_handle__{}__msg__{}", name.package, name.name);
        // SAFETY: rosidl generates this function with exactly this signature for every message type
        let handle = unsafe {
            let get = library
                .get::<unsafe extern "C" fn() -> *const rosidl_message_type_support_t>(symbol.as_bytes())
                .with_context(|| format!("{} has no type {}", name.package, name.name))?;
            get()
        };
        if handle.is_null() {
            anyhow::bail!("no type support for {ty}");
        }
        self.handles.insert(ty.to_owned(), handle);
        Ok(handle)
    }
}

/// Opens a package's typesupport library from `AMENT_PREFIX_PATH`, or failing that from the loader's search path
fn open(package: &str) -> anyhow::Result<Library> {
    let file = format!("lib{package}__rosidl_typesupport_c.so");
    let prefixes = std::env::var_os("AMENT_PREFIX_PATH").unwrap_or_default();
    let path = std::env::split_paths(&prefixes)
        .map(|prefix| prefix.join("lib").join(&file))
        .find(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from(&file));
    // SAFETY: a typesupport library's initializers only register its types
    unsafe { Library::new(&path) }
        .with_context(|| format!("couldn't load {file}; is {package} installed and its environment sourced?"))
}
