//! Finds and caches message definitions. In an installed ROS environment, `pkg/msg/Type` is defined in
//! `<prefix>/share/<pkg>/msg/<Type>.msg` for one of the prefixes in `AMENT_PREFIX_PATH`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, anyhow};

use crate::schema::{self, Base, MessageDef, TypeName};

#[derive(Default)]
pub struct Registry {
    /// Directories holding one directory per package
    share_dirs: Vec<PathBuf>,
    defs: HashMap<TypeName, Arc<MessageDef>>,
}

impl Registry {
    pub fn new(share_dirs: Vec<PathBuf>) -> Self {
        Self { share_dirs, defs: HashMap::new() }
    }

    /// The `share` directories of every prefix in `AMENT_PREFIX_PATH`
    pub fn from_env() -> Self {
        let prefixes = std::env::var_os("AMENT_PREFIX_PATH").unwrap_or_default();
        Self::new(std::env::split_paths(&prefixes).map(|prefix| prefix.join("share")).collect())
    }

    pub fn add_share_dir(&mut self, dir: PathBuf) {
        self.share_dirs.push(dir);
    }

    /// Loads a type and every type it contains, so [`Registry::get`] can find them all while decoding
    pub fn load(&mut self, name: &TypeName) -> anyhow::Result<Arc<MessageDef>> {
        if let Some(def) = self.defs.get(name) {
            return Ok(def.clone());
        }
        let path = self.find(name).ok_or_else(|| {
            anyhow!("no definition of {name} (looked for {}/msg/{}.msg in AMENT_PREFIX_PATH)", name.package, name.name)
        })?;
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let def = Arc::new(schema::parse(name, &text)?);
        self.defs.insert(name.clone(), def.clone());
        for field in &def.fields {
            if let Base::Message(nested) = &field.base {
                self.load(nested).with_context(|| format!("in field `{}` of {name}", field.name))?;
            }
        }
        Ok(def)
    }

    /// A type already loaded, directly or as part of another
    pub fn get(&self, name: &TypeName) -> Option<&MessageDef> {
        self.defs.get(name).map(Arc::as_ref)
    }

    fn find(&self, name: &TypeName) -> Option<PathBuf> {
        self.share_dirs
            .iter()
            .map(|dir| dir.join(&name.package).join("msg").join(format!("{}.msg", name.name)))
            .find(|path| path.is_file())
    }

    /// Every message type installed, for checking that they all parse
    pub fn installed(&self) -> Vec<TypeName> {
        let mut names = Vec::new();
        for dir in &self.share_dirs {
            let Ok(packages) = std::fs::read_dir(dir) else { continue };
            for package in packages.flatten() {
                let Ok(files) = std::fs::read_dir(package.path().join("msg")) else { continue };
                for file in files.flatten() {
                    let path = file.path();
                    if path.extension().is_some_and(|e| e == "msg")
                        && let (Some(pkg), Some(stem)) = (package.file_name().to_str(), file_stem(&path))
                    {
                        names.extend(TypeName::parse(&format!("{pkg}/{stem}")));
                    }
                }
            }
        }
        names
    }
}

fn file_stem(path: &Path) -> Option<&str> {
    path.file_stem().and_then(|s| s.to_str())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A share directory with the given `pkg/Type` definitions, removed when dropped
    pub struct FakeShare(pub PathBuf);

    impl FakeShare {
        pub fn new(defs: &[(&str, &str)]) -> Self {
            static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let dir = std::env::temp_dir().join(format!("archon-msg-test-{}-{n}", std::process::id()));
            for (name, text) in defs {
                let (pkg, ty) = name.split_once('/').unwrap();
                let msg_dir = dir.join(pkg).join("msg");
                std::fs::create_dir_all(&msg_dir).unwrap();
                std::fs::write(msg_dir.join(format!("{ty}.msg")), text).unwrap();
            }
            FakeShare(dir)
        }

        pub fn registry(&self) -> Registry {
            Registry::new(vec![self.0.clone()])
        }
    }

    impl Drop for FakeShare {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn loads_nested_types() {
        let share = FakeShare::new(&[
            ("std_msgs/Header", "builtin_interfaces/Time stamp\nstring frame_id\n"),
            ("builtin_interfaces/Time", "int32 sec\nuint32 nanosec\n"),
            ("demo/Reading", "Header header\nfloat64 value\n"),
        ]);
        let mut registry = share.registry();
        registry.load(&TypeName::parse("demo/msg/Reading").unwrap()).unwrap();
        assert!(registry.get(&TypeName::parse("builtin_interfaces/Time").unwrap()).is_some());
        assert_eq!(registry.installed().len(), 3);
    }

    #[test]
    fn missing_nested_type_says_where() {
        let share = FakeShare::new(&[("demo/Outer", "demo/Missing inner\n")]);
        let err = share.registry().load(&TypeName::parse("demo/Outer").unwrap()).unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("field `inner`") && text.contains("demo/msg/Missing"), "{text}");
    }

    /// Every message in the ROS environment parses and resolves. Skipped outside one.
    #[test]
    fn every_installed_message_loads() {
        if std::env::var_os("AMENT_PREFIX_PATH").is_none() {
            return;
        }
        let mut registry = Registry::from_env();
        let installed = registry.installed();
        assert!(installed.len() > 50, "found only {} messages", installed.len());
        let failures: Vec<String> =
            installed.iter().filter_map(|name| registry.load(name).err().map(|e| format!("{e:#}"))).collect();
        assert!(failures.is_empty(), "{} of {} failed:\n{}", failures.len(), installed.len(), failures.join("\n"));
    }
}
