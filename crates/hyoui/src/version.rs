//! Build version identity shared by CLI and HTTP surfaces.

use serde::{Deserialize, Serialize};

/// A hyoui build identity.
///
/// `Deserialize` も持つのは、読み手が居るため: gateway の `GET /version` の応答と、
/// 監督者の制御 socket に載る版を CLI が読み返す (DR-0034 決定 4 / 7a)。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct VersionInfo {
    /// Crate version from `Cargo.toml`.
    pub version: String,
    /// Build identifier supplied by the build script, when available.
    pub build_id: Option<String>,
}

impl VersionInfo {
    /// Returns the identity embedded in this process.
    pub fn current() -> Self {
        Self {
            version: crate::VERSION.to_string(),
            build_id: crate::BUILD_ID.map(str::to_string),
        }
    }

    /// Formats the one-line `hyoui --version` output.
    pub fn display_line(&self) -> String {
        match &self.build_id {
            Some(build_id) => format!("hyoui {} ({build_id})", self.version),
            None => format!("hyoui {}", self.version),
        }
    }
}

/// Parses one line of `hyoui --version` output.
///
/// Outputs that do not identify themselves as `hyoui` or do not match either
/// supported shape return `None`.
pub fn parse_version_line(line: &str) -> Option<VersionInfo> {
    let rest = line.strip_prefix("hyoui ")?;
    if rest.is_empty() || rest != rest.trim() {
        return None;
    }

    if let Some(without_suffix) = rest.strip_suffix(')') {
        let (version, build_id) = without_suffix.rsplit_once(" (")?;
        if version.is_empty()
            || build_id.is_empty()
            || version.chars().any(char::is_whitespace)
            || build_id.chars().any(char::is_whitespace)
        {
            return None;
        }
        return Some(VersionInfo {
            version: version.to_string(),
            build_id: Some(build_id.to_string()),
        });
    }

    if rest.chars().any(char::is_whitespace) {
        return None;
    }
    Some(VersionInfo {
        version: rest.to_string(),
        build_id: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_version_with_build_id() {
        assert_eq!(
            parse_version_line("hyoui 0.9.44 (a1b2c3d-dirty)"),
            Some(VersionInfo {
                version: "0.9.44".to_string(),
                build_id: Some("a1b2c3d-dirty".to_string()),
            })
        );
    }

    #[test]
    fn parses_version_without_build_id() {
        assert_eq!(
            parse_version_line("hyoui 0.9.44"),
            Some(VersionInfo {
                version: "0.9.44".to_string(),
                build_id: None,
            })
        );
    }

    #[test]
    fn rejects_other_binary_name() {
        assert_eq!(parse_version_line("something-else 0.9.44"), None);
    }
}
