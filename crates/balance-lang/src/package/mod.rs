use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A dependency specification.
#[derive(Debug, Clone)]
pub enum DepSpec {
    /// Version string (e.g. "1.0")
    Version(String),
    /// Path dependency with optional version constraint (e.g. { path = "../kv", version = "^1.0" })
    Path(PathBuf, Option<String>),
}

/// A parsed balance.toml manifest.
#[derive(Debug, Clone)]
pub struct PackageManifest {
    pub name: String,
    pub version: String,
    pub dependencies: HashMap<String, DepSpec>,
}

impl PackageManifest {
    /// Parse a balance.toml file.
    pub fn from_file(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read '{}': {e}", path.display()))?;
        Self::from_str(&content)
    }

    pub fn from_str(content: &str) -> Result<Self, String> {
        let table: toml::Table =
            content.parse().map_err(|e| format!("invalid TOML: {e}"))?;

        let name = table
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
            .ok_or("missing [package] name")?
            .to_string();

        let version = table
            .get("package")
            .and_then(|p| p.get("version"))
            .and_then(|v| v.as_str())
            .unwrap_or("0.1.0")
            .to_string();

        let mut dependencies = HashMap::new();
        if let Some(deps) = table.get("dependencies").and_then(|d| d.as_table()) {
            for (k, v) in deps {
                let dep = if let Some(s) = v.as_str() {
                    DepSpec::Version(s.to_string())
                } else if let Some(t) = v.as_table() {
                    if let Some(path) = t.get("path").and_then(|p| p.as_str()) {
                        let version = t.get("version").and_then(|v| v.as_str()).map(|s| s.to_string());
                        DepSpec::Path(PathBuf::from(path), version)
                    } else {
                        DepSpec::Version(
                            t.get("version")
                                .and_then(|v| v.as_str())
                                .unwrap_or("*")
                                .to_string(),
                        )
                    }
                } else {
                    DepSpec::Version("*".to_string())
                };
                dependencies.insert(k.clone(), dep);
            }
        }

        Ok(Self {
            name,
            version,
            dependencies,
        })
    }

    /// Find a balance.toml by walking up from the given directory.
    pub fn find(start: &Path) -> Option<PathBuf> {
        let mut dir = start.to_path_buf();
        loop {
            let manifest = dir.join("balance.toml");
            if manifest.exists() {
                return Some(manifest);
            }
            if !dir.pop() {
                return None;
            }
        }
    }
}

/// Semantic version with major.minor.patch components.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemVer {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl SemVer {
    /// Parse a version string like "1.2.3", "1.2", or "1".
    pub fn parse(s: &str) -> Result<Self, String> {
        let parts: Vec<&str> = s.trim().split('.').collect();
        if parts.is_empty() || parts.len() > 3 {
            return Err(format!("invalid semver: '{s}'"));
        }
        let major = parts[0]
            .parse::<u32>()
            .map_err(|_| format!("invalid major version in '{s}'"))?;
        let minor = if parts.len() > 1 {
            parts[1]
                .parse::<u32>()
                .map_err(|_| format!("invalid minor version in '{s}'"))?
        } else {
            0
        };
        let patch = if parts.len() > 2 {
            parts[2]
                .parse::<u32>()
                .map_err(|_| format!("invalid patch version in '{s}'"))?
        } else {
            0
        };
        Ok(Self { major, minor, patch })
    }
}

impl Ord for SemVer {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.major
            .cmp(&other.major)
            .then(self.minor.cmp(&other.minor))
            .then(self.patch.cmp(&other.patch))
    }
}

impl PartialOrd for SemVer {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for SemVer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Version constraint for dependency resolution.
#[derive(Debug, Clone, PartialEq)]
pub enum VersionConstraint {
    /// Exact version: "=1.2.3" or "1.2.3"
    Exact(SemVer),
    /// Compatible version: "^1.2.3" — same major, minor >= specified, patch any
    Compatible(SemVer),
    /// Greater-than-or-equal: ">=1.2.3"
    Gte(SemVer),
    /// Any version: "*"
    Any,
}

impl VersionConstraint {
    /// Parse a constraint string.
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s == "*" {
            return Ok(Self::Any);
        }
        if let Some(rest) = s.strip_prefix(">=") {
            let ver = SemVer::parse(rest.trim())?;
            return Ok(Self::Gte(ver));
        }
        if let Some(rest) = s.strip_prefix('^') {
            let ver = SemVer::parse(rest.trim())?;
            return Ok(Self::Compatible(ver));
        }
        if let Some(rest) = s.strip_prefix('=') {
            let ver = SemVer::parse(rest.trim())?;
            return Ok(Self::Exact(ver));
        }
        // Default: treat bare version as compatible (like Cargo)
        let ver = SemVer::parse(s)?;
        Ok(Self::Compatible(ver))
    }

    /// Check if a version satisfies this constraint.
    pub fn matches(&self, ver: &SemVer) -> bool {
        match self {
            Self::Any => true,
            Self::Exact(target) => ver == target,
            Self::Gte(min) => ver >= min,
            Self::Compatible(target) => {
                if target.major == 0 {
                    // 0.x: minor must match exactly, patch >= target
                    ver.major == 0 && ver.minor == target.minor && ver.patch >= target.patch
                } else {
                    // >=1.x: same major, (minor, patch) >= target
                    ver.major == target.major
                        && (ver.minor > target.minor
                            || (ver.minor == target.minor && ver.patch >= target.patch))
                }
            }
        }
    }
}

impl DepSpec {
    /// Parse the version string into a `VersionConstraint`.
    pub fn parsed_constraint(&self) -> Result<VersionConstraint, String> {
        match self {
            DepSpec::Version(s) => VersionConstraint::parse(s),
            DepSpec::Path(_, Some(version)) => VersionConstraint::parse(version),
            DepSpec::Path(_, None) => Ok(VersionConstraint::Any),
        }
    }
}

/// Resolve dependencies against a set of available packages.
/// Returns the subset of `available` packages that satisfy all constraints.
pub fn resolve_dependencies(
    manifest: &PackageManifest,
    available: &[PackageManifest],
) -> Result<Vec<PackageManifest>, String> {
    let mut resolved = Vec::new();

    for (dep_name, dep_spec) in &manifest.dependencies {
        let constraint = dep_spec.parsed_constraint()?;

        // Find matching packages
        let candidates: Vec<&PackageManifest> = available
            .iter()
            .filter(|pkg| {
                if pkg.name != *dep_name {
                    return false;
                }
                if let Ok(ver) = SemVer::parse(&pkg.version) {
                    constraint.matches(&ver)
                } else {
                    false
                }
            })
            .collect();

        if candidates.is_empty() {
            if matches!(dep_spec, DepSpec::Path(_, _)) {
                // Path dependencies don't need to be in the available set
                continue;
            }
            return Err(format!(
                "no package '{dep_name}' found matching constraint '{}'",
                match dep_spec {
                    DepSpec::Version(v) => v.as_str(),
                    DepSpec::Path(_, _) => "*",
                }
            ));
        }

        // Pick the highest matching version
        let best = candidates
            .iter()
            .max_by_key(|pkg| SemVer::parse(&pkg.version).unwrap_or(SemVer { major: 0, minor: 0, patch: 0 }))
            .unwrap();

        resolved.push((*best).clone());
    }

    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_manifest() {
        let content = r#"
[package]
name = "my-app"
version = "0.2.0"

[dependencies]
kv = "1.0"
logging = "0.3"
"#;
        let manifest = PackageManifest::from_str(content).unwrap();
        assert_eq!(manifest.name, "my-app");
        assert_eq!(manifest.version, "0.2.0");
        assert_eq!(manifest.dependencies.len(), 2);
        assert!(matches!(
            manifest.dependencies.get("kv"),
            Some(DepSpec::Version(v)) if v == "1.0"
        ));
        assert!(matches!(
            manifest.dependencies.get("logging"),
            Some(DepSpec::Version(v)) if v == "0.3"
        ));
    }

    #[test]
    fn test_parse_path_dependency() {
        let content = r#"
[package]
name = "my-app"
version = "0.1.0"

[dependencies]
kv = { path = "../kv" }
logging = "0.3"
"#;
        let manifest = PackageManifest::from_str(content).unwrap();
        assert_eq!(manifest.dependencies.len(), 2);
        assert!(matches!(
            manifest.dependencies.get("kv"),
            Some(DepSpec::Path(p, _)) if p == Path::new("../kv")
        ));
        assert!(matches!(
            manifest.dependencies.get("logging"),
            Some(DepSpec::Version(v)) if v == "0.3"
        ));
    }

    #[test]
    fn test_parse_minimal_manifest() {
        let content = r#"
[package]
name = "simple"
"#;
        let manifest = PackageManifest::from_str(content).unwrap();
        assert_eq!(manifest.name, "simple");
        assert_eq!(manifest.version, "0.1.0");
        assert!(manifest.dependencies.is_empty());
    }

    #[test]
    fn test_missing_package_name_errors() {
        let content = r#"
[package]
version = "1.0"
"#;
        let result = PackageManifest::from_str(content);
        assert!(result.is_err());
    }

    #[test]
    fn test_find_manifest() {
        let dir = std::env::temp_dir().join("balance_pkg_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/deep")).unwrap();
        std::fs::write(
            dir.join("balance.toml"),
            "[package]\nname = \"test\"\n",
        )
        .unwrap();

        let found = PackageManifest::find(&dir.join("src/deep"));
        assert!(found.is_some());
        assert!(found.unwrap().ends_with("balance.toml"));
    }

    #[test]
    fn test_semver_parse_full() {
        let v = SemVer::parse("1.2.3").unwrap();
        assert_eq!(v, SemVer { major: 1, minor: 2, patch: 3 });
    }

    #[test]
    fn test_semver_parse_partial() {
        let v = SemVer::parse("2.1").unwrap();
        assert_eq!(v, SemVer { major: 2, minor: 1, patch: 0 });

        let v2 = SemVer::parse("3").unwrap();
        assert_eq!(v2, SemVer { major: 3, minor: 0, patch: 0 });
    }

    #[test]
    fn test_semver_ordering() {
        let v1 = SemVer::parse("1.0.0").unwrap();
        let v2 = SemVer::parse("1.1.0").unwrap();
        let v3 = SemVer::parse("2.0.0").unwrap();
        assert!(v1 < v2);
        assert!(v2 < v3);
        assert!(v1 < v3);
    }

    #[test]
    fn test_constraint_exact() {
        let c = VersionConstraint::parse("=1.2.3").unwrap();
        assert!(c.matches(&SemVer::parse("1.2.3").unwrap()));
        assert!(!c.matches(&SemVer::parse("1.2.4").unwrap()));
        assert!(!c.matches(&SemVer::parse("1.3.0").unwrap()));
    }

    #[test]
    fn test_constraint_compatible() {
        let c = VersionConstraint::parse("^1.2.0").unwrap();
        assert!(c.matches(&SemVer::parse("1.2.0").unwrap()));
        assert!(c.matches(&SemVer::parse("1.3.0").unwrap()));
        assert!(c.matches(&SemVer::parse("1.9.9").unwrap()));
        assert!(!c.matches(&SemVer::parse("2.0.0").unwrap()));
        assert!(!c.matches(&SemVer::parse("1.1.0").unwrap()));
    }

    #[test]
    fn test_constraint_gte() {
        let c = VersionConstraint::parse(">=1.5.0").unwrap();
        assert!(c.matches(&SemVer::parse("1.5.0").unwrap()));
        assert!(c.matches(&SemVer::parse("2.0.0").unwrap()));
        assert!(!c.matches(&SemVer::parse("1.4.9").unwrap()));
    }

    #[test]
    fn test_constraint_any() {
        let c = VersionConstraint::parse("*").unwrap();
        assert!(c.matches(&SemVer::parse("0.0.1").unwrap()));
        assert!(c.matches(&SemVer::parse("99.99.99").unwrap()));
    }

    #[test]
    fn test_constraint_bare_version_is_compatible() {
        // Bare "1.0" is treated as ^1.0.0 (like Cargo)
        let c = VersionConstraint::parse("1.0").unwrap();
        assert_eq!(c, VersionConstraint::Compatible(SemVer { major: 1, minor: 0, patch: 0 }));
        assert!(c.matches(&SemVer::parse("1.5.0").unwrap()));
        assert!(!c.matches(&SemVer::parse("2.0.0").unwrap()));
    }

    #[test]
    fn test_resolve_dependencies_simple() {
        let manifest = PackageManifest::from_str(
            r#"
[package]
name = "my-app"
version = "1.0.0"

[dependencies]
kv = "1.0"
"#,
        )
        .unwrap();

        let available = vec![
            PackageManifest {
                name: "kv".into(),
                version: "1.0.0".into(),
                dependencies: HashMap::new(),
            },
            PackageManifest {
                name: "kv".into(),
                version: "1.2.0".into(),
                dependencies: HashMap::new(),
            },
            PackageManifest {
                name: "kv".into(),
                version: "2.0.0".into(),
                dependencies: HashMap::new(),
            },
        ];

        let resolved = resolve_dependencies(&manifest, &available).unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].name, "kv");
        // Should pick highest compatible: 1.2.0 (not 2.0.0)
        assert_eq!(resolved[0].version, "1.2.0");
    }

    #[test]
    fn test_resolve_dependencies_conflict() {
        let manifest = PackageManifest::from_str(
            r#"
[package]
name = "my-app"
version = "1.0.0"

[dependencies]
kv = "^3.0"
"#,
        )
        .unwrap();

        let available = vec![PackageManifest {
            name: "kv".into(),
            version: "2.0.0".into(),
            dependencies: HashMap::new(),
        }];

        let result = resolve_dependencies(&manifest, &available);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("no package 'kv'"));
    }
}
