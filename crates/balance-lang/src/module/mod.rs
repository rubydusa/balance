use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::ast::*;
use crate::lexer::tokenize;
use crate::parser::parse;

/// A loaded module with its declarations and metadata.
#[derive(Debug)]
pub struct Module {
    pub name: String,
    pub path: PathBuf,
    pub program: Program,
    pub exported_ports: HashSet<String>,
    pub exported_services: HashSet<String>,
    pub exported_types: HashSet<String>,
    pub exported_fns: HashSet<String>,
}

/// Multi-file module loader.
pub struct ModuleLoader {
    /// Root directory for resolving imports.
    root: PathBuf,
    /// Loaded modules keyed by module path (e.g., "kv.storage").
    modules: HashMap<String, Module>,
    /// Track loading order for cycle detection.
    loading: HashSet<String>,
    /// Parent manifest for version constraint enforcement.
    parent_manifest: Option<crate::package::PackageManifest>,
}

impl ModuleLoader {
    pub fn new(root: PathBuf) -> Self {
        // Auto-detect and parse balance.toml from root for version constraint enforcement
        let parent_manifest = crate::package::PackageManifest::find(&root)
            .and_then(|p| crate::package::PackageManifest::from_file(&p).ok());
        Self {
            root,
            modules: HashMap::new(),
            loading: HashSet::new(),
            parent_manifest,
        }
    }

    /// Load a file and all its transitive imports.
    pub fn load_file(&mut self, file_path: &Path) -> Result<String, String> {
        let source = std::fs::read_to_string(file_path)
            .map_err(|e| format!("cannot read '{}': {e}", file_path.display()))?;

        let tokens = tokenize(&source).map_err(|spans| {
            format!(
                "tokenize errors in '{}': {} errors",
                file_path.display(),
                spans.len()
            )
        })?;

        let (program, parse_errors) = parse(&source, &tokens);
        if !parse_errors.is_empty() {
            return Err(format!(
                "parse errors in '{}': {}",
                file_path.display(),
                parse_errors
                    .iter()
                    .map(|e| e.display(&source))
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }

        // Determine module name
        let mod_name = if let Some(ref decl) = program.module_decl {
            decl.path.join(".")
        } else {
            // Derive from file path relative to root
            // e.g., root=/project, file=/project/kv/types.bl -> "kv.types"
            let relative = file_path
                .strip_prefix(&self.root)
                .unwrap_or(file_path);
            let stem = relative.with_extension("");
            stem.components()
                .filter_map(|c| {
                    if let std::path::Component::Normal(s) = c {
                        Some(s.to_string_lossy().to_string())
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join(".")
        };

        // Cycle detection
        if self.loading.contains(&mod_name) {
            return Err(format!("circular import detected: module '{mod_name}'"));
        }
        if self.modules.contains_key(&mod_name) {
            return Ok(mod_name);
        }
        self.loading.insert(mod_name.clone());

        // Collect exports
        let mut exported_ports = HashSet::new();
        let mut exported_services = HashSet::new();
        let mut exported_types = HashSet::new();
        let mut exported_fns = HashSet::new();

        for item in &program.items {
            match &item.node {
                Item::Port(p) if p.exported => {
                    exported_ports.insert(p.name.clone());
                }
                Item::Service(s) if s.exported => {
                    exported_services.insert(s.name.clone());
                }
                Item::TypeDecl(t) if t.exported => {
                    exported_types.insert(t.name.clone());
                }
                Item::FnDecl(f) => {
                    // Functions are exported by default (like Rust pub fn in lib)
                    exported_fns.insert(f.name.clone());
                }
                _ => {}
            }
        }

        // Resolve imports transitively
        let imports = program.imports.clone();
        let module = Module {
            name: mod_name.clone(),
            path: file_path.to_path_buf(),
            program,
            exported_ports,
            exported_services,
            exported_types,
            exported_fns,
        };
        self.modules.insert(mod_name.clone(), module);

        for import in &imports {
            self.resolve_import(&import.node, file_path)?;
        }

        self.loading.remove(&mod_name);
        Ok(mod_name)
    }

    /// Resolve an import declaration by locating and loading the referenced module.
    ///
    /// **Circular import policy**: Port-reference cycles are allowed.
    /// When module A imports a port from module B and vice versa, this is
    /// permitted because port declarations are pure interfaces with no runtime
    /// dependencies. Service, entry, function, and type cycles are still rejected.
    fn resolve_import(&mut self, import: &ImportDecl, from_file: &Path) -> Result<(), String> {
        let mod_path = import.path.join(".");

        // Check for circular import: if module is currently being loaded
        if self.loading.contains(&mod_path) {
            // Port-reference cycles are allowed: if the partially-loaded module
            // is already in self.modules and the import only references ports,
            // permit the cycle since port declarations are pure interfaces.
            if let Some(target_module) = self.modules.get(&mod_path) {
                let is_port_only_import = if let Some(ref names) = import.names {
                    // Selective import: check that all requested names are exported ports
                    names.iter().all(|(name, _alias)| target_module.exported_ports.contains(name))
                } else {
                    // Non-selective import: allow only if module exports at least one port
                    // and nothing else (services, types, functions)
                    !target_module.exported_ports.is_empty()
                        && target_module.exported_services.is_empty()
                        && target_module.exported_types.is_empty()
                        && target_module.exported_fns.is_empty()
                };
                if is_port_only_import {
                    return Ok(());
                }
            }
            return Err(format!("circular import detected: module '{mod_path}'"));
        }

        if self.modules.contains_key(&mod_path) {
            return Ok(());
        }

        // Resolve file path: "kv.storage" -> look for "kv/storage.bl"
        let relative: PathBuf = import
            .path
            .iter()
            .collect::<PathBuf>()
            .with_extension("bl");

        // Try relative to project root
        let file_path = self.root.join(&relative);
        if file_path.exists() {
            self.load_file(&file_path)?;
            return Ok(());
        }

        // Try relative to the importing file's directory
        if let Some(parent) = from_file.parent() {
            let file_path = parent.join(&relative);
            if file_path.exists() {
                self.load_file(&file_path)?;
                return Ok(());
            }
        }

        // Try balance.toml path dependencies
        let first_segment = &import.path[0];
        if let Some(manifest_path) = crate::package::PackageManifest::find(&self.root) {
            if let Ok(manifest) = crate::package::PackageManifest::from_file(&manifest_path) {
                if let Some(crate::package::DepSpec::Path(dep_path, _)) =
                    manifest.dependencies.get(first_segment)
                {
                    let manifest_dir = manifest_path
                        .parent()
                        .unwrap_or_else(|| Path::new("."));
                    let dep_root = manifest_dir.join(dep_path);
                    // For import "kv.types", first segment is "kv" (the dependency name),
                    // remaining segments map to the file inside the dep directory
                    let sub_path: PathBuf = if import.path.len() > 1 {
                        import.path[1..]
                            .iter()
                            .collect::<PathBuf>()
                            .with_extension("bl")
                    } else {
                        PathBuf::from("lib.bl")
                    };
                    let dep_file = dep_root.join(&sub_path);
                    if dep_file.exists() {
                        // Validate version constraint if the dependency has its own balance.toml
                        if let Some(ref parent) = self.parent_manifest {
                            if let Some(dep_spec) = parent.dependencies.get(first_segment) {
                                let dep_manifest_path = dep_root.join("balance.toml");
                                if dep_manifest_path.exists() {
                                    if let Ok(dep_manifest) =
                                        crate::package::PackageManifest::from_file(&dep_manifest_path)
                                    {
                                        if let Ok(constraint) = dep_spec.parsed_constraint() {
                                            if let Ok(ver) =
                                                crate::package::SemVer::parse(&dep_manifest.version)
                                            {
                                                if !constraint.matches(&ver) {
                                                    return Err(format!(
                                                        "dependency '{}' version {} does not satisfy constraint (from parent manifest)",
                                                        first_segment, dep_manifest.version
                                                    ));
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        self.load_file(&dep_file)?;
                        return Ok(());
                    }
                }
            }
        }

        Err(format!(
            "cannot resolve import '{}': file not found (looked for '{}')",
            mod_path,
            relative.display()
        ))
    }

    /// Get all loaded modules.
    pub fn modules(&self) -> &HashMap<String, Module> {
        &self.modules
    }

    /// Get a specific module.
    pub fn get_module(&self, name: &str) -> Option<&Module> {
        self.modules.get(name)
    }

    /// Check if a symbol is exported from a module.
    pub fn is_exported(&self, module_name: &str, symbol: &str) -> bool {
        self.modules
            .get(module_name)
            .map(|m| {
                m.exported_ports.contains(symbol)
                    || m.exported_services.contains(symbol)
                    || m.exported_types.contains(symbol)
                    || m.exported_fns.contains(symbol)
            })
            .unwrap_or(false)
    }

    /// Collect exported symbols from a module as a flat map of name → symbol kind.
    /// Used for qualified name resolution: `import kv.storage as storage` → `storage.KV`.
    pub fn exported_symbol_names(&self, module_name: &str) -> Vec<String> {
        let Some(module) = self.modules.get(module_name) else {
            return Vec::new();
        };
        let mut names = Vec::new();
        for name in &module.exported_ports {
            names.push(name.clone());
        }
        for name in &module.exported_services {
            names.push(name.clone());
        }
        for name in &module.exported_types {
            names.push(name.clone());
        }
        for name in &module.exported_fns {
            names.push(name.clone());
        }
        names
    }

    /// Resolve imported symbols for a module, returning merged items.
    pub fn resolve_imports(&self, module_name: &str) -> Result<Vec<Item>, String> {
        let module = self
            .modules
            .get(module_name)
            .ok_or_else(|| format!("module '{module_name}' not loaded"))?;

        let mut imported_items = Vec::new();

        for import in &module.program.imports {
            let imp = &import.node;
            let source_mod_name = imp.path.join(".");
            let source_module = self
                .modules
                .get(&source_mod_name)
                .ok_or_else(|| format!("imported module '{source_mod_name}' not loaded"))?;

            if let Some(names) = &imp.names {
                // Selective import: import kv.storage.{KV, Logger as L}
                for (name, per_name_alias) in names {
                    if !self.is_exported(&source_mod_name, name) {
                        return Err(format!(
                            "symbol '{name}' is not exported from module '{source_mod_name}'"
                        ));
                    }
                    // Find and include the matching item
                    for item in &source_module.program.items {
                        let item_name = item_decl_name(&item.node);
                        if item_name.as_deref() == Some(name.as_str()) {
                            // Per-name alias takes precedence over import-level alias
                            if let Some(ref alias) = per_name_alias {
                                imported_items.push(rename_item(&item.node, alias));
                            } else if let Some(ref alias) = imp.alias {
                                if names.len() == 1 {
                                    imported_items.push(rename_item(&item.node, alias));
                                } else {
                                    imported_items.push(item.node.clone());
                                }
                            } else {
                                imported_items.push(item.node.clone());
                            }
                        }
                    }
                }
            } else if let Some(ref alias) = imp.alias {
                // Import with alias: import kv.types as KV
                // Import all exported items, renaming the primary export
                let mut first = true;
                for item in &source_module.program.items {
                    if let Some(name) = item_decl_name(&item.node) {
                        if self.is_exported(&source_mod_name, &name) {
                            if first {
                                imported_items.push(rename_item(&item.node, alias));
                                first = false;
                            } else {
                                imported_items.push(item.node.clone());
                            }
                        }
                    }
                }
            } else {
                // Import all exported symbols
                for item in &source_module.program.items {
                    if let Some(name) = item_decl_name(&item.node) {
                        if self.is_exported(&source_mod_name, &name) {
                            imported_items.push(item.node.clone());
                        }
                    }
                }
            }
        }

        Ok(imported_items)
    }
}

fn item_decl_name(item: &Item) -> Option<String> {
    match item {
        Item::Port(p) => Some(p.name.clone()),
        Item::Service(s) => Some(s.name.clone()),
        Item::TypeDecl(t) => Some(t.name.clone()),
        Item::FnDecl(f) => Some(f.name.clone()),
        Item::Substrate(s) => Some(s.name.clone()),
        Item::Guarantee(g) => Some(g.name.clone()),
        Item::Profile(p) => Some(p.name.clone()),
        Item::Macro(m) => Some(m.name.clone()),
        _ => None,
    }
}

/// Rename an item's primary identifier (used for import aliases).
pub fn rename_item(item: &Item, new_name: &str) -> Item {
    match item {
        Item::Port(p) => Item::Port(PortDecl {
            name: new_name.to_string(),
            ..p.clone()
        }),
        Item::Service(s) => Item::Service(ServiceDecl {
            name: new_name.to_string(),
            ..s.clone()
        }),
        Item::TypeDecl(t) => Item::TypeDecl(TypeDecl {
            name: new_name.to_string(),
            ..t.clone()
        }),
        Item::FnDecl(f) => Item::FnDecl(FnDecl {
            name: new_name.to_string(),
            ..f.clone()
        }),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("balance_module_test_{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_load_single_file() {
        let dir = setup_test_dir("single");
        let file = dir.join("hello.bl");
        fs::write(&file, r#""Hello, World!""#).unwrap();

        let mut loader = ModuleLoader::new(dir.clone());
        let name = loader.load_file(&file).unwrap();
        assert_eq!(name, "hello");
        assert!(loader.get_module("hello").is_some());
    }

    #[test]
    fn test_load_with_explicit_module_name() {
        let dir = setup_test_dir("explicit");
        let file = dir.join("mymod.bl");
        fs::write(
            &file,
            r#"module custom.name
            "hello""#,
        )
        .unwrap();

        let mut loader = ModuleLoader::new(dir.clone());
        let name = loader.load_file(&file).unwrap();
        assert_eq!(name, "custom.name");
    }

    #[test]
    fn test_export_tracking() {
        let dir = setup_test_dir("export");
        let file = dir.join("ports.bl");
        fs::write(
            &file,
            r#"
            export port KV {
                get(key: String) -> String? [query]
            }
            port Internal {
                secret() -> String [query]
            }
            "#,
        )
        .unwrap();

        let mut loader = ModuleLoader::new(dir.clone());
        loader.load_file(&file).unwrap();

        assert!(loader.is_exported("ports", "KV"));
        assert!(!loader.is_exported("ports", "Internal"));
    }

    #[test]
    fn test_multi_file_import() {
        let dir = setup_test_dir("multi");
        fs::create_dir_all(dir.join("kv")).unwrap();

        fs::write(
            dir.join("kv/types.bl"),
            r#"
            export port KV {
                get(key: String) -> String? [query]
            }
            "#,
        )
        .unwrap();

        fs::write(
            dir.join("app.bl"),
            r#"
            import kv.types
            entry() {
                let kv = resolve KV["kv/main"]
                kv.get("hello")
            }
            "#,
        )
        .unwrap();

        let mut loader = ModuleLoader::new(dir.clone());
        let name = loader.load_file(&dir.join("app.bl")).unwrap();
        assert_eq!(name, "app");
        assert!(loader.get_module("kv.types").is_some());
    }

    #[test]
    fn test_circular_import_detected() {
        let sub = setup_test_dir("circular");

        fs::write(
            sub.join("alpha.bl"),
            r#"
            import beta
            "#,
        )
        .unwrap();

        fs::write(
            sub.join("beta.bl"),
            r#"
            import alpha
            "#,
        )
        .unwrap();

        let mut loader = ModuleLoader::new(sub.clone());
        let result = loader.load_file(&sub.join("alpha.bl"));
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("circular import"));
    }

    #[test]
    fn test_private_symbol_not_accessible() {
        let dir = setup_test_dir("private");

        fs::write(
            dir.join("lib.bl"),
            r#"
            port Internal {
                secret() -> String [query]
            }
            export port Public {
                hello() -> String [query]
            }
            "#,
        )
        .unwrap();

        let mut loader = ModuleLoader::new(dir.clone());
        loader.load_file(&dir.join("lib.bl")).unwrap();

        assert!(loader.is_exported("lib", "Public"));
        assert!(!loader.is_exported("lib", "Internal"));
    }

    #[test]
    fn test_exported_symbol_names() {
        let dir = setup_test_dir("symbols");

        fs::write(
            dir.join("lib.bl"),
            r#"
            export port KV {
                get(key: String) -> String? [query]
            }
            export port Logger {
                log(msg: String) -> Unit [command]
            }
            port Internal {
                secret() -> String [query]
            }
            "#,
        )
        .unwrap();

        let mut loader = ModuleLoader::new(dir.clone());
        loader.load_file(&dir.join("lib.bl")).unwrap();

        let names = loader.exported_symbol_names("lib");
        assert!(names.contains(&"KV".to_string()));
        assert!(names.contains(&"Logger".to_string()));
        assert!(!names.contains(&"Internal".to_string()));
    }

    #[test]
    fn test_cross_module_port_import() {
        let dir = setup_test_dir("cross_port");
        fs::create_dir_all(dir.join("helpers")).unwrap();

        // Module exporting a port
        fs::write(
            dir.join("helpers/greeter.bl"),
            r#"
            export port Greeter {
                greet(name: String) -> String [query]
            }
            "#,
        )
        .unwrap();

        // Main module importing the port
        fs::write(
            dir.join("main.bl"),
            r#"
            import helpers.greeter.{Greeter}
            entry() {
                let g = resolve Greeter["greeter/default"]
                await g.greet("world")
            }
            "#,
        )
        .unwrap();

        let mut loader = ModuleLoader::new(dir.clone());
        let name = loader.load_file(&dir.join("main.bl")).unwrap();
        assert_eq!(name, "main");

        // The greeter module should be loaded transitively
        assert!(loader.get_module("helpers.greeter").is_some());

        // Greeter port should be exported
        assert!(loader.is_exported("helpers.greeter", "Greeter"));

        // Resolve imports should include the Greeter port
        let imported_items = loader.resolve_imports(&name).unwrap();
        let has_greeter = imported_items.iter().any(|item| {
            matches!(item, Item::Port(p) if p.name == "Greeter")
        });
        assert!(has_greeter, "expected Greeter port in imported items");
    }

    #[test]
    fn test_cross_module_service_import() {
        let dir = setup_test_dir("cross_service");
        fs::create_dir_all(dir.join("helpers")).unwrap();

        // Module exporting both a port and a service
        fs::write(
            dir.join("helpers/greeter.bl"),
            r#"
            export port Greeter {
                greet(name: String) -> String [query]
            }
            export service GreeterService provides Greeter {
                publish as "greeter/default"
                query greet(name: String) -> String {
                    return "Hello, " + name + "!"
                }
            }
            "#,
        )
        .unwrap();

        // Main module importing port + service
        fs::write(
            dir.join("main.bl"),
            r#"
            import helpers.greeter.{Greeter, GreeterService}
            entry() {
                let g = resolve Greeter["greeter/default"]
                await g.greet("world")
            }
            "#,
        )
        .unwrap();

        let mut loader = ModuleLoader::new(dir.clone());
        let name = loader.load_file(&dir.join("main.bl")).unwrap();

        let imported_items = loader.resolve_imports(&name).unwrap();
        let has_port = imported_items.iter().any(|item| {
            matches!(item, Item::Port(p) if p.name == "Greeter")
        });
        let has_service = imported_items.iter().any(|item| {
            matches!(item, Item::Service(s) if s.name == "GreeterService")
        });
        assert!(has_port, "expected Greeter port in imported items");
        assert!(has_service, "expected GreeterService in imported items");
    }

    #[test]
    fn test_aliased_selective_import() {
        let dir = setup_test_dir("aliased_selective");
        fs::create_dir_all(dir.join("helpers")).unwrap();

        fs::write(
            dir.join("helpers/math.bl"),
            r#"
            export fn add(a: Int, b: Int) -> Int { return a + b }
            export fn mul(a: Int, b: Int) -> Int { return a * b }
            "#,
        )
        .unwrap();

        fs::write(
            dir.join("main.bl"),
            r#"
            import helpers.math.{add as plus, mul}
            entry() {
                plus(1, 2)
            }
            "#,
        )
        .unwrap();

        let mut loader = ModuleLoader::new(dir.clone());
        let name = loader.load_file(&dir.join("main.bl")).unwrap();

        let imported_items = loader.resolve_imports(&name).unwrap();
        let has_plus = imported_items.iter().any(|item| {
            matches!(item, Item::FnDecl(f) if f.name == "plus")
        });
        let has_mul = imported_items.iter().any(|item| {
            matches!(item, Item::FnDecl(f) if f.name == "mul")
        });
        assert!(has_plus, "expected 'add' to be renamed to 'plus'");
        assert!(has_mul, "expected 'mul' to be imported unchanged");
    }

    #[test]
    fn test_aliased_mixed_import() {
        let dir = setup_test_dir("aliased_mixed");
        fs::create_dir_all(dir.join("helpers")).unwrap();

        fs::write(
            dir.join("helpers/greeter.bl"),
            r#"
            export port Greeter {
                greet(name: String) -> String [query]
            }
            export service GreeterService provides Greeter {
                publish as "greeter/default"
                query greet(name: String) -> String {
                    return "Hello, " + name + "!"
                }
            }
            "#,
        )
        .unwrap();

        fs::write(
            dir.join("main.bl"),
            r#"
            import helpers.greeter.{Greeter as G, GreeterService}
            entry() {
                let g = resolve G["greeter/default"]
                await g.greet("world")
            }
            "#,
        )
        .unwrap();

        let mut loader = ModuleLoader::new(dir.clone());
        let name = loader.load_file(&dir.join("main.bl")).unwrap();

        let imported_items = loader.resolve_imports(&name).unwrap();
        let has_g = imported_items.iter().any(|item| {
            matches!(item, Item::Port(p) if p.name == "G")
        });
        let has_svc = imported_items.iter().any(|item| {
            matches!(item, Item::Service(s) if s.name == "GreeterService")
        });
        assert!(has_g, "expected Greeter to be renamed to G");
        assert!(has_svc, "expected GreeterService imported unchanged");
    }

    // === Gap 8: Version constraint enforcement at import time ===

    #[test]
    fn test_version_constraint_matching_loads_ok() {
        let dir = setup_test_dir("ver_ok");
        fs::create_dir_all(dir.join("mylib")).unwrap();

        // Parent balance.toml requires mylib ^1.0
        fs::write(
            dir.join("balance.toml"),
            r#"[package]
name = "app"
version = "1.0.0"

[dependencies]
mylib = { path = "mylib" }
"#,
        )
        .unwrap();

        // Dep balance.toml has version 1.2.0 (satisfies ^1.0)
        fs::write(
            dir.join("mylib/balance.toml"),
            r#"[package]
name = "mylib"
version = "1.2.0"
"#,
        )
        .unwrap();

        fs::write(
            dir.join("mylib/lib.bl"),
            r#"export fn helper() -> Int { return 42 }"#,
        )
        .unwrap();

        fs::write(
            dir.join("main.bl"),
            r#"import mylib
            entry() { return 1 }"#,
        )
        .unwrap();

        let mut loader = ModuleLoader::new(dir.clone());
        let result = loader.load_file(&dir.join("main.bl"));
        assert!(result.is_ok(), "expected version-matching dep to load: {:?}", result);
    }

    #[test]
    fn test_version_constraint_mismatch_errors() {
        let dir = setup_test_dir("ver_mismatch");
        fs::create_dir_all(dir.join("mylib")).unwrap();

        // Parent balance.toml requires mylib ^2.0
        fs::write(
            dir.join("balance.toml"),
            r#"[package]
name = "app"
version = "1.0.0"

[dependencies]
mylib = { path = "mylib", version = "^2.0" }
"#,
        )
        .unwrap();

        // Dep balance.toml has version 1.0.0 (does NOT satisfy ^2.0)
        fs::write(
            dir.join("mylib/balance.toml"),
            r#"[package]
name = "mylib"
version = "1.0.0"
"#,
        )
        .unwrap();

        fs::write(
            dir.join("mylib/lib.bl"),
            r#"export fn helper() -> Int { return 42 }"#,
        )
        .unwrap();

        fs::write(
            dir.join("main.bl"),
            r#"import mylib
            entry() { return 1 }"#,
        )
        .unwrap();

        let mut loader = ModuleLoader::new(dir.clone());
        let result = loader.load_file(&dir.join("main.bl"));
        assert!(result.is_err(), "expected version mismatch error");
        assert!(
            result.unwrap_err().contains("does not satisfy"),
            "expected 'does not satisfy' in error message"
        );
    }

    #[test]
    fn test_path_dep_without_balance_toml_loads_ok() {
        let dir = setup_test_dir("ver_no_toml");
        fs::create_dir_all(dir.join("mylib")).unwrap();

        // Parent balance.toml with path dep
        fs::write(
            dir.join("balance.toml"),
            r#"[package]
name = "app"
version = "1.0.0"

[dependencies]
mylib = { path = "mylib" }
"#,
        )
        .unwrap();

        // No balance.toml in mylib — should load fine
        fs::write(
            dir.join("mylib/lib.bl"),
            r#"export fn helper() -> Int { return 42 }"#,
        )
        .unwrap();

        fs::write(
            dir.join("main.bl"),
            r#"import mylib
            entry() { return 1 }"#,
        )
        .unwrap();

        let mut loader = ModuleLoader::new(dir.clone());
        let result = loader.load_file(&dir.join("main.bl"));
        assert!(result.is_ok(), "expected dep without balance.toml to load: {:?}", result);
    }

    #[test]
    fn test_port_reference_cycle_allowed() {
        // Module A exports a port, module B exports a port, both import each other's port.
        // Port-only cycles should be allowed.
        let dir = setup_test_dir("port_cycle");

        fs::write(
            dir.join("alpha.bl"),
            r#"
            import beta.{BetaPort}
            export port AlphaPort {
                greet() -> String [query]
            }
            "#,
        )
        .unwrap();

        fs::write(
            dir.join("beta.bl"),
            r#"
            import alpha.{AlphaPort}
            export port BetaPort {
                hello() -> String [query]
            }
            "#,
        )
        .unwrap();

        let mut loader = ModuleLoader::new(dir.clone());
        let result = loader.load_file(&dir.join("alpha.bl"));
        assert!(
            result.is_ok(),
            "port-reference cycle should be allowed, got: {:?}",
            result
        );
    }

    #[test]
    fn test_service_cycle_still_rejected() {
        // Module A imports a service from B, B imports from A — this should still be rejected.
        let dir = setup_test_dir("service_cycle");

        fs::write(
            dir.join("alpha.bl"),
            r#"
            import beta.{BetaSvc}
            export port AlphaPort {
                greet() -> String [query]
            }
            fn use_beta() -> String { return "x" }
            "#,
        )
        .unwrap();

        fs::write(
            dir.join("beta.bl"),
            r#"
            import alpha
            export service BetaSvc provides BetaPort {
                publish as "beta/main"
                query hello() -> String { return "hi" }
            }
            export port BetaPort {
                hello() -> String [query]
            }
            "#,
        )
        .unwrap();

        let mut loader = ModuleLoader::new(dir.clone());
        let result = loader.load_file(&dir.join("alpha.bl"));
        // beta exports a service AND port, so non-selective import of alpha creates a
        // non-port cycle (beta has exported_services non-empty)
        assert!(
            result.is_err(),
            "service cycle should still be rejected"
        );
        assert!(
            result.unwrap_err().contains("circular import"),
            "expected circular import error"
        );
    }

    #[test]
    fn test_mixed_cycle_selective_port_only_allowed() {
        // Module A exports port + service, B selectively imports only the port from A.
        // This should be allowed since the selective import is port-only.
        let dir = setup_test_dir("mixed_selective");

        fs::write(
            dir.join("alpha.bl"),
            r#"
            import beta.{BetaPort}
            export port AlphaPort {
                greet() -> String [query]
            }
            export service AlphaSvc provides AlphaPort {
                publish as "alpha/main"
                query greet() -> String { return "hi" }
            }
            "#,
        )
        .unwrap();

        fs::write(
            dir.join("beta.bl"),
            r#"
            import alpha.{AlphaPort}
            export port BetaPort {
                hello() -> String [query]
            }
            "#,
        )
        .unwrap();

        let mut loader = ModuleLoader::new(dir.clone());
        let result = loader.load_file(&dir.join("alpha.bl"));
        assert!(
            result.is_ok(),
            "selective port-only import in cycle should be allowed, got: {:?}",
            result
        );
    }
}
