//! Extension bundle on disk.
//!
//! Accepts three input shapes:
//! - an unpacked directory containing `manifest.json` at the root
//! - a `.zip` or `.xpi` archive (extracted to a temp dir on load)
//! - a `.crx` archive (Chrome MV3 — has a small header before the zip body
//!   which we strip; see <https://developer.chrome.com/docs/apps/crx>)
//!
//! After load, all files are kept in memory as raw bytes — real-world
//! extension bundles are small (~hundreds of KB unpacked) and we read
//! scripts many times per page load.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::manifest::{ExtensionManifest, ManifestError};

#[derive(Debug, Error)]
pub enum BundleError {
    #[error("io error reading bundle: {0}")]
    Io(#[from] std::io::Error),
    #[error("zip error reading bundle: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("manifest error: {0}")]
    Manifest(#[from] ManifestError),
    #[error("unsupported bundle format: {0}")]
    Unsupported(String),
    #[error("malformed .crx header")]
    CrxHeader,
}

/// All files in the extension bundle, keyed by relative path with forward
/// slashes (matches both manifest references and `executeScript({files})`
/// API expectations).
pub struct Bundle {
    pub manifest: ExtensionManifest,
    pub source_path: PathBuf,
    pub files: HashMap<String, Vec<u8>>,
}

impl Bundle {
    /// Load from any supported input. Auto-detects shape from filesystem.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, BundleError> {
        let path = path.as_ref();
        let md = std::fs::metadata(path)?;
        if md.is_dir() {
            Self::load_dir(path)
        } else {
            let bytes = std::fs::read(path)?;
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            match ext.as_str() {
                "zip" | "xpi" => Self::load_zip(path, &bytes),
                "crx" => Self::load_crx(path, &bytes),
                other => Err(BundleError::Unsupported(other.into())),
            }
        }
    }

    fn load_dir(root: &Path) -> Result<Self, BundleError> {
        let manifest_path = root.join("manifest.json");
        let manifest_json = std::fs::read_to_string(&manifest_path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                BundleError::Manifest(ManifestError::Missing)
            } else {
                BundleError::Io(e)
            }
        })?;
        let manifest = ExtensionManifest::parse(&manifest_json)?;

        let mut files = HashMap::new();
        // Real-world bundles top out at a few hundred files; walking is
        // fine. We skip dotfiles so a directory that was also a git
        // checkout doesn't drag `.git/` into memory.
        walk_dir(root, root, &mut files)?;
        Ok(Self {
            manifest,
            source_path: root.to_path_buf(),
            files,
        })
    }

    fn load_zip(source: &Path, bytes: &[u8]) -> Result<Self, BundleError> {
        let reader = std::io::Cursor::new(bytes);
        let mut zip = zip::ZipArchive::new(reader)?;
        let mut files: HashMap<String, Vec<u8>> = HashMap::new();
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            if entry.is_dir() {
                continue;
            }
            let raw_name = entry
                .enclosed_name()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|| entry.name().to_string());
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            files.insert(raw_name, buf);
        }

        // Pick the canonical `manifest.json`.
        //
        // Some real-world bundles ship MORE THAN ONE `manifest.json`:
        // GitHub-style source archives wrap everything in
        // `<repo>-<branch>/manifest.json`, signed `.xpi` / `.crx`
        // packages put it at the root, and a few extensions (notably
        // Bypass Paywalls Clean) embed a second mini-bundle under
        // `custom/manifest.json` that overlays the primary one.
        //
        // We canonicalise on the SHALLOWEST manifest — root wins over
        // any sub-directory. Ties broken by lexicographic path order
        // for determinism. The previous heuristic ("first hit, two
        // independent passes through a `HashMap`") was non-deterministic
        // across HashMap seedings: ~50% of runs picked the deeper
        // `custom/manifest.json`, then stripped `custom/` from every
        // key — which dropped most of the bundle and silently
        // disabled the extension. See `picks_shallowest_manifest`.
        let chosen_manifest_path = files
            .keys()
            .filter(|k| k.ends_with("manifest.json") && (k.as_str() == "manifest.json" || k.contains('/')))
            .min_by(|a, b| {
                let da = a.matches('/').count();
                let db = b.matches('/').count();
                da.cmp(&db).then_with(|| a.cmp(b))
            })
            .cloned()
            .ok_or(BundleError::Manifest(ManifestError::Missing))?;

        let manifest_bytes = files
            .get(&chosen_manifest_path)
            .ok_or(BundleError::Manifest(ManifestError::Missing))?;
        let manifest_json = String::from_utf8_lossy(manifest_bytes).into_owned();
        let manifest = ExtensionManifest::parse(&manifest_json)?;

        // Strip the chosen manifest's parent directory from every key
        // so that paths in the registry match what the manifest
        // references (e.g. `background.js`, not
        // `bypass-paywalls-firefox-clean-master/background.js`). We
        // reuse the EXACT path we parsed the manifest from — never
        // re-discover it from the keys collection — so the prefix is
        // guaranteed consistent with the chosen manifest.
        let prefix = chosen_manifest_path
            .rsplit_once('/')
            .map(|(parent, _)| format!("{parent}/"))
            .unwrap_or_default();
        let files = if prefix.is_empty() {
            files
        } else {
            files
                .into_iter()
                .filter_map(|(k, v)| k.strip_prefix(&prefix).map(|s| (s.to_string(), v)))
                .collect()
        };

        Ok(Self {
            manifest,
            source_path: source.to_path_buf(),
            files,
        })
    }

    fn load_crx(source: &Path, bytes: &[u8]) -> Result<Self, BundleError> {
        // CRX2: 4-byte magic `Cr24`, u32 version, u32 pubkey_len, u32 sig_len,
        //       then pubkey + sig + zip body.
        // CRX3: 4-byte magic `Cr24`, u32 version=3, u32 header_size,
        //       then a serialised header proto, then zip body.
        if bytes.len() < 16 || &bytes[..4] != b"Cr24" {
            return Err(BundleError::CrxHeader);
        }
        let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        let zip_start = match version {
            2 => {
                let pubkey_len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
                let sig_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
                16 + pubkey_len + sig_len
            }
            3 => {
                let header_size = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
                12 + header_size
            }
            _ => return Err(BundleError::CrxHeader),
        };
        if zip_start >= bytes.len() {
            return Err(BundleError::CrxHeader);
        }
        Self::load_zip(source, &bytes[zip_start..])
    }

    /// Get a file by extension-relative path. The path is normalised to use
    /// forward slashes regardless of input style.
    pub fn get(&self, rel: &str) -> Option<&[u8]> {
        let normalised = rel.trim_start_matches('/').replace('\\', "/");
        self.files.get(&normalised).map(|v| v.as_slice())
    }

    /// Get a file as UTF-8 text; convenience for script files.
    pub fn get_text(&self, rel: &str) -> Option<String> {
        self.get(rel).map(|b| String::from_utf8_lossy(b).into_owned())
    }

    pub fn has(&self, rel: &str) -> bool {
        self.get(rel).is_some()
    }
}

fn walk_dir(
    root: &Path,
    dir: &Path,
    out: &mut HashMap<String, Vec<u8>>,
) -> Result<(), std::io::Error> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            // Skip dotfiles (.git, .gitignore, .DS_Store).
            continue;
        }
        if path.is_dir() {
            walk_dir(root, &path, out)?;
        } else {
            let rel = path
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| name.to_string());
            let bytes = std::fs::read(&path)?;
            out.insert(rel, bytes);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a tiny in-memory MV2 bundle as a zip. Used by every test
    /// below so we don't need a real third-party extension on disk to
    /// exercise the loader. The bundle has:
    ///   - manifest.json (MV2, two background scripts)
    ///   - background/main.js (string body)
    ///   - background/util.js
    ///   - content/cs.js
    ///   - assets/icon.png (bytes that aren't JS)
    fn synth_zip_bytes() -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut buf);
        let opts: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let manifest = r#"{
            "manifest_version": 2,
            "name": "Synth",
            "version": "1.2.3",
            "background": { "scripts": ["background/util.js", "background/main.js"] },
            "permissions": ["cookies", "storage", "*://*.example.com/*"]
        }"#;
        zip.start_file("manifest.json", opts).unwrap();
        zip.write_all(manifest.as_bytes()).unwrap();
        zip.start_file("background/main.js", opts).unwrap();
        zip.write_all(b"console.log('main');").unwrap();
        zip.start_file("background/util.js", opts).unwrap();
        zip.write_all(b"function util(){}").unwrap();
        zip.start_file("content/cs.js", opts).unwrap();
        zip.write_all(b"document.title = 'cs';").unwrap();
        zip.start_file("assets/icon.png", opts).unwrap();
        zip.write_all(&[0x89, 0x50, 0x4e, 0x47]).unwrap();
        zip.finish().unwrap(); // moves zip; drops the borrow on buf
        buf.into_inner()
    }

    /// Write a synth bundle to a tempfile and return its path. The
    /// caller's responsibility to keep the temp dir alive.
    fn write_synth_zip(suffix: &str) -> (tempdir::TempDir, std::path::PathBuf) {
        let dir = tempdir::TempDir::new("obscura-ext-test").unwrap();
        let path = dir.path().join(format!("synth.{suffix}"));
        std::fs::write(&path, synth_zip_bytes()).unwrap();
        (dir, path)
    }

    #[test]
    fn loads_synth_zip() {
        let (_g, path) = write_synth_zip("zip");
        let bundle = Bundle::load(&path).expect("zip load");
        assert_eq!(bundle.manifest.manifest_version, 2);
        assert_eq!(bundle.manifest.name, "Synth");
        assert_eq!(bundle.manifest.version, "1.2.3");
        assert_eq!(
            bundle.manifest.background_scripts,
            vec!["background/util.js", "background/main.js"]
        );
        assert_eq!(bundle.manifest.permissions, vec!["cookies", "storage"]);
        assert_eq!(
            bundle.manifest.host_patterns,
            vec!["*://*.example.com/*"]
        );
        assert!(bundle.has("background/main.js"));
        assert!(bundle.has("content/cs.js"));
        assert!(bundle.has("assets/icon.png"));
        assert!(!bundle.has("does-not-exist.js"));
        assert_eq!(bundle.get_text("background/main.js").as_deref(), Some("console.log('main');"));
    }

    #[test]
    fn loads_synth_xpi_alias_for_zip() {
        // Firefox releases bundles as `.xpi` which is structurally a
        // zip. The loader treats `.xpi` and `.zip` identically.
        let (_g, path) = write_synth_zip("xpi");
        let bundle = Bundle::load(&path).expect("xpi load");
        assert_eq!(bundle.manifest.manifest_version, 2);
        assert!(bundle.has("background/main.js"));
    }

    #[test]
    fn loads_synth_unpacked_directory() {
        let dir = tempdir::TempDir::new("obscura-ext-test").unwrap();
        std::fs::write(
            dir.path().join("manifest.json"),
            r#"{ "manifest_version": 2, "name": "Synth", "version": "1.0",
                 "background": { "scripts": ["main.js"] } }"#,
        )
        .unwrap();
        std::fs::write(dir.path().join("main.js"), "// hi").unwrap();
        let bundle = Bundle::load(dir.path()).expect("dir load");
        assert_eq!(bundle.manifest.manifest_version, 2);
        assert!(bundle.has("main.js"));
    }

    #[test]
    fn loads_synth_crx_v3() {
        // Build a CRX3: 4-byte magic `Cr24`, u32 version=3, u32 header_size,
        // header_size bytes of "header", then the zip body. The header
        // contents don't matter to our loader (we don't verify the
        // signature), only the byte offsets.
        let zip_bytes = synth_zip_bytes();
        let header = b"OBSCURA-TEST-HEADER";
        let mut crx = Vec::new();
        crx.extend_from_slice(b"Cr24");
        crx.extend_from_slice(&3u32.to_le_bytes());
        crx.extend_from_slice(&(header.len() as u32).to_le_bytes());
        crx.extend_from_slice(header);
        crx.extend_from_slice(&zip_bytes);

        let dir = tempdir::TempDir::new("obscura-ext-test").unwrap();
        let path = dir.path().join("synth.crx");
        std::fs::write(&path, &crx).unwrap();
        let bundle = Bundle::load(&path).expect("crx load");
        assert_eq!(bundle.manifest.manifest_version, 2);
        assert!(bundle.has("background/main.js"));
    }

    #[test]
    fn rejects_unknown_extension() {
        let dir = tempdir::TempDir::new("obscura-ext-test").unwrap();
        let path = dir.path().join("synth.tar.gz");
        std::fs::write(&path, b"not a zip").unwrap();
        assert!(matches!(
            Bundle::load(&path),
            Err(BundleError::Unsupported(_))
        ));
    }

    /// Build a bundle that embeds a SECOND `manifest.json` under a
    /// `custom/` sub-directory. The shape mirrors Bypass Paywalls
    /// Clean's `.xpi`, which ships a primary manifest at the root and a
    /// stripped-down overlay manifest at `custom/manifest.json`.
    ///
    /// We run the load 32 times in a row to exercise HashMap iteration
    /// non-determinism (which was the underlying source of the
    /// "sometimes the extension does nothing" symptom). The fix selects
    /// the SHALLOWEST manifest deterministically, so every load must
    /// pick the root manifest and see every bundled file.
    #[test]
    fn picks_shallowest_manifest_when_multiple_present() {
        fn build() -> Vec<u8> {
            let mut buf = std::io::Cursor::new(Vec::new());
            let mut zip = zip::ZipWriter::new(&mut buf);
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            // Root manifest — the real one. Two background scripts.
            let root_manifest = r#"{
                "manifest_version": 2,
                "name": "Multi",
                "version": "1.0",
                "background": { "scripts": ["sites.js", "background.js"] },
                "permissions": ["cookies", "*://*.example.com/*"]
            }"#;
            zip.start_file("manifest.json", opts).unwrap();
            zip.write_all(root_manifest.as_bytes()).unwrap();
            zip.start_file("sites.js", opts).unwrap();
            zip.write_all(b"var defaultSites = {};").unwrap();
            zip.start_file("background.js", opts).unwrap();
            zip.write_all(b"console.log('bg');").unwrap();
            zip.start_file("contentScript.js", opts).unwrap();
            zip.write_all(b"// cs").unwrap();
            // Overlay manifest under custom/ — narrower scope, no host gate.
            let custom_manifest = r#"{
                "manifest_version": 2,
                "name": "Multi (custom)",
                "version": "1.0",
                "background": { "scripts": ["sites.js", "background.js"] },
                "permissions": ["cookies", "*://*/*"]
            }"#;
            zip.start_file("custom/manifest.json", opts).unwrap();
            zip.write_all(custom_manifest.as_bytes()).unwrap();
            zip.start_file("custom/sites_custom.json", opts).unwrap();
            zip.write_all(b"{}").unwrap();
            zip.finish().unwrap();
            buf.into_inner()
        }

        let dir = tempdir::TempDir::new("obscura-ext-test").unwrap();
        let path = dir.path().join("multi.xpi");
        std::fs::write(&path, build()).unwrap();

        // 32 reloads in one process don't reseed HashMap, but the
        // selector itself must not depend on iteration order. We also
        // re-run in a fresh sub-process loop in CI; this in-process
        // loop catches any "first key wins" regressions.
        for i in 0..32 {
            let bundle = Bundle::load(&path).unwrap_or_else(|e| panic!("iter {i}: {e}"));
            assert_eq!(bundle.manifest.name, "Multi", "iter {i}: wrong manifest");
            assert_eq!(
                bundle.manifest.host_patterns,
                vec!["*://*.example.com/*"],
                "iter {i}: wrong host patterns (overlay manifest was chosen)"
            );
            assert!(bundle.has("sites.js"), "iter {i}: sites.js missing");
            assert!(bundle.has("background.js"), "iter {i}: background.js missing");
            assert!(bundle.has("contentScript.js"), "iter {i}: contentScript.js missing");
            // The overlay manifest survives at its original path —
            // we don't strip `custom/` away.
            assert!(
                bundle.has("custom/manifest.json"),
                "iter {i}: custom/ overlay missing"
            );
        }
    }
}
