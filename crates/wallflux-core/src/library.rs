//! The wallpaper library: an open, inspectable package format.
//!
//! A wallpaper is a directory holding a `wallflux.toml` manifest plus its
//! assets. Nothing is compiled, encrypted or indexed in a database, so a
//! wallpaper can be shared as a plain zip or a git repo and edited by hand:
//!
//! ```text
//! neon-city/
//! ├── wallflux.toml
//! ├── scene.mp4
//! └── preview.jpg      # optional; generated into the cache when absent
//! ```
//!
//! ```toml
//! [wallpaper]
//! title   = "Neon City"
//! author  = "someone"
//! license = "CC-BY-4.0"
//! kind    = "video"
//! tags    = ["city", "night"]
//!
//! [video]
//! file = "scene.mp4"
//! ```
//!
//! A bare video file dropped in the library directory also counts as a
//! wallpaper (an *implicit* package), so trying wallflux out never requires
//! writing a manifest first.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config::ScaleMode;
use crate::paths;

/// Extensions accepted for implicit packages and manifest `file` fields.
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mkv", "webm", "mov", "m4v", "avi", "gif"];

const MANIFEST_NAME: &str = "wallflux.toml";
const PREVIEW_NAMES: &[&str] = &[
    "preview.jpg",
    "preview.jpeg",
    "preview.png",
    "preview.webp",
    "thumbnail.jpg",
    "thumbnail.png",
];

/// What kind of content a wallpaper renders.
///
/// v1 ships `Video` only. The variants are an enum rather than a bare string
/// so adding `Shader` and `Web` later is a compile-time exhaustiveness error
/// everywhere it matters instead of a runtime surprise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum WallpaperKind {
    #[default]
    Video,
}

/// The `[wallpaper]` table of a manifest.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ManifestMeta {
    /// Defaults to the directory name, which is what you want almost always.
    id: Option<String>,
    title: Option<String>,
    author: Option<String>,
    license: Option<String>,
    /// Free-form, used by the GUI for filtering.
    tags: Vec<String>,
    kind: WallpaperKind,
}

/// The `[video]` table of a manifest.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ManifestVideo {
    /// Relative to the package directory. When absent, the only video file in
    /// the directory is used, so a one-file package needs no `file` line.
    file: Option<String>,
    /// Per-wallpaper overrides, applied on top of the user's config.
    fps_cap: Option<u32>,
    scale_mode: Option<ScaleMode>,
    mute: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    #[serde(default)]
    wallpaper: ManifestMeta,
    #[serde(default)]
    video: ManifestVideo,
}

/// A wallpaper resolved against the filesystem: every path here exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wallpaper {
    /// Stable identifier used in config and IPC. Unique within a library.
    pub id: String,
    pub title: String,
    pub author: Option<String>,
    pub license: Option<String>,
    pub tags: Vec<String>,
    pub kind: WallpaperKind,
    /// The package directory, or the parent directory for implicit packages.
    pub root: PathBuf,
    /// The asset the daemon feeds to mpv.
    pub source: PathBuf,
    /// A still image for the GUI grid, when the package ships one.
    pub preview: Option<PathBuf>,
    pub fps_cap: Option<u32>,
    pub scale_mode: Option<ScaleMode>,
    pub mute: Option<bool>,
    /// True when there is no manifest and the entry was inferred from a file.
    pub implicit: bool,
}

impl Wallpaper {
    /// Loads a wallpaper from a package directory.
    pub fn load(dir: &Path) -> Result<Self> {
        let manifest_path = dir.join(MANIFEST_NAME);
        let text = std::fs::read_to_string(&manifest_path)
            .with_context(|| format!("cannot read {}", manifest_path.display()))?;
        let manifest: Manifest = toml::from_str(&text)
            .with_context(|| format!("invalid manifest {}", manifest_path.display()))?;

        let dir_name = dir
            .file_name()
            .and_then(|name| name.to_str())
            .context("package directory has a non-UTF-8 name")?;
        let id = manifest
            .wallpaper
            .id
            .clone()
            .unwrap_or_else(|| dir_name.to_string());
        validate_id(&id)?;

        let source = match &manifest.video.file {
            Some(file) => {
                let path = dir.join(file);
                if !path.is_file() {
                    bail!(
                        "{}: video.file points at {}, which does not exist",
                        manifest_path.display(),
                        path.display()
                    );
                }
                path
            }
            None => sole_video_in(dir)?.with_context(|| {
                format!(
                    "{}: no video.file given and no video file found in the package",
                    manifest_path.display()
                )
            })?,
        };

        Ok(Self {
            title: manifest
                .wallpaper
                .title
                .clone()
                .unwrap_or_else(|| humanise(&id)),
            id,
            author: manifest.wallpaper.author,
            license: manifest.wallpaper.license,
            tags: manifest.wallpaper.tags,
            kind: manifest.wallpaper.kind,
            preview: find_preview(dir),
            root: dir.to_path_buf(),
            source,
            fps_cap: manifest.video.fps_cap,
            scale_mode: manifest.video.scale_mode,
            mute: manifest.video.mute,
            implicit: false,
        })
    }

    /// Builds an implicit wallpaper from a loose video file.
    pub fn from_video_file(path: &Path) -> Result<Self> {
        let stem = path
            .file_stem()
            .and_then(|name| name.to_str())
            .context("video file has a non-UTF-8 name")?;
        let id = slugify(stem);
        validate_id(&id)?;
        Ok(Self {
            id,
            title: humanise(stem),
            author: None,
            license: None,
            tags: Vec::new(),
            kind: WallpaperKind::Video,
            root: path.parent().unwrap_or(Path::new(".")).to_path_buf(),
            source: path.to_path_buf(),
            preview: None,
            fps_cap: None,
            scale_mode: None,
            mute: None,
            implicit: true,
        })
    }
}

/// All wallpapers discovered under a library directory.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Library {
    pub root: PathBuf,
    pub wallpapers: Vec<Wallpaper>,
    /// Packages that failed to load, as `(path, reason)`. Surfaced in the GUI
    /// instead of being swallowed: a broken manifest the user cannot see is
    /// worse than one listed with its error.
    pub errors: Vec<(PathBuf, String)>,
}

impl Library {
    /// Scans the default library directory, creating it when missing.
    pub fn load() -> Result<Self> {
        let root = paths::library_dir()?;
        paths::ensure_dir(&root)?;
        Self::load_from(&root)
    }

    pub fn load_from(root: &Path) -> Result<Self> {
        let mut library = Self {
            root: root.to_path_buf(),
            ..Default::default()
        };

        let entries = match std::fs::read_dir(root) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(library),
            Err(err) => {
                return Err(err).with_context(|| format!("cannot read {}", root.display()));
            }
        };

        for entry in entries {
            let entry = entry.with_context(|| format!("cannot walk {}", root.display()))?;
            let path = entry.path();
            let result = if path.is_dir() {
                if path.join(MANIFEST_NAME).is_file() {
                    Some(Wallpaper::load(&path))
                } else {
                    // A directory without a manifest holding exactly one video
                    // still works: unzip-and-go should not need a manifest.
                    match sole_video_in(&path) {
                        Ok(Some(video)) => {
                            Some(Wallpaper::from_video_file(&video).map(|mut wp| {
                                wp.root = path.clone();
                                wp.preview = find_preview(&path);
                                wp
                            }))
                        }
                        Ok(None) => None,
                        Err(err) => Some(Err(err)),
                    }
                }
            } else if is_video_file(&path) {
                Some(Wallpaper::from_video_file(&path))
            } else {
                None
            };

            match result {
                Some(Ok(wallpaper)) => library.wallpapers.push(wallpaper),
                Some(Err(err)) => library.errors.push((path, format!("{err:#}"))),
                None => {}
            }
        }

        library.dedupe_ids();
        library.wallpapers.sort_by(|a, b| a.title.cmp(&b.title));
        Ok(library)
    }

    pub fn get(&self, id: &str) -> Option<&Wallpaper> {
        self.wallpapers.iter().find(|wp| wp.id == id)
    }

    /// Two packages can legitimately claim the same id (same folder name under
    /// different parents, or a manifest colliding with a loose file). Rather
    /// than drop one, later duplicates are suffixed so both stay reachable.
    fn dedupe_ids(&mut self) {
        let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for wallpaper in &mut self.wallpapers {
            let count = seen.entry(wallpaper.id.clone()).or_insert(0);
            *count += 1;
            if *count > 1 {
                wallpaper.id = format!("{}-{}", wallpaper.id, count);
            }
        }
    }
}

fn is_video_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            let ext = ext.to_ascii_lowercase();
            VIDEO_EXTENSIONS.contains(&ext.as_str())
        })
        .unwrap_or(false)
}

/// The single video file in `dir`, or `None` when there are none. More than
/// one is an error: guessing which the user meant would be a coin flip.
fn sole_video_in(dir: &Path) -> Result<Option<PathBuf>> {
    let mut found = Vec::new();
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("cannot read {}", dir.display()))?;
    for entry in entries {
        let path = entry?.path();
        if is_video_file(&path) {
            found.push(path);
        }
    }
    found.sort();
    match found.len() {
        0 => Ok(None),
        1 => Ok(Some(found.remove(0))),
        n => bail!(
            "{} holds {n} video files; add a [video] file = \"…\" line to {MANIFEST_NAME} \
             to say which one to use",
            dir.display()
        ),
    }
}

fn find_preview(dir: &Path) -> Option<PathBuf> {
    PREVIEW_NAMES
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file())
}

/// Ids end up in file paths, config keys and IPC, so they stay boring.
fn validate_id(id: &str) -> Result<()> {
    if id.is_empty() {
        bail!("wallpaper id is empty");
    }
    if id == "." || id == ".." {
        bail!("wallpaper id {id:?} is reserved");
    }
    if let Some(bad) = id
        .chars()
        .find(|c| !c.is_ascii_alphanumeric() && !matches!(c, '-' | '_' | '.'))
    {
        bail!("wallpaper id {id:?} contains an unsupported character {bad:?}");
    }
    Ok(())
}

/// Turns a file name into an id: `my_cool-Wallpaper 2` → `my_cool-wallpaper-2`.
///
/// Public because the GUI slugifies names too when importing, and two copies
/// of this would drift until an imported wallpaper got an id the daemon could
/// not find.
pub fn slugify(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
            slug.push(ch.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_matches('-').to_string()
}

/// `neon-city_02` → `Neon City 02`, for a readable default title.
fn humanise(name: &str) -> String {
    name.split(['-', '_', ' '])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "wallflux-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, b"not really a video").unwrap();
    }

    #[test]
    fn slugify_and_humanise_are_sane() {
        assert_eq!(slugify("my_cool-Wallpaper 2"), "my_cool-wallpaper-2");
        assert_eq!(slugify("Neon City 02.mp4"), "neon-city-02-mp4");
        // Underscores survive: they are legal in an id, so there is nothing
        // to replace them with.
        assert_eq!(slugify("___"), "___");
        assert_eq!(humanise("neon-city_02"), "Neon City 02");
    }

    #[test]
    fn id_validation_rejects_path_traversal() {
        assert!(validate_id("..").is_err());
        assert!(validate_id("a/b").is_err());
        assert!(validate_id("ok-id_1.2").is_ok());
    }

    #[test]
    fn loose_video_file_becomes_an_implicit_wallpaper() {
        let dir = tempdir();
        touch(&dir.join("Neon City.mp4"));
        let library = Library::load_from(&dir).unwrap();
        assert_eq!(library.wallpapers.len(), 1);
        let wallpaper = &library.wallpapers[0];
        assert_eq!(wallpaper.id, "neon-city");
        assert_eq!(wallpaper.title, "Neon City");
        assert!(wallpaper.implicit);
    }

    #[test]
    fn manifest_package_is_loaded_with_metadata() {
        let dir = tempdir();
        let pkg = dir.join("neon-city");
        touch(&pkg.join("scene.mp4"));
        touch(&pkg.join("preview.jpg"));
        std::fs::write(
            pkg.join(MANIFEST_NAME),
            r#"
            [wallpaper]
            title = "Neon City"
            author = "someone"
            license = "CC-BY-4.0"
            tags = ["city", "night"]

            [video]
            file = "scene.mp4"
            fps_cap = 24
            "#,
        )
        .unwrap();

        let library = Library::load_from(&dir).unwrap();
        assert!(library.errors.is_empty(), "{:?}", library.errors);
        let wallpaper = library.get("neon-city").expect("wallpaper missing");
        assert_eq!(wallpaper.author.as_deref(), Some("someone"));
        assert_eq!(wallpaper.tags, vec!["city", "night"]);
        assert_eq!(wallpaper.fps_cap, Some(24));
        assert!(wallpaper.preview.is_some());
        assert!(!wallpaper.implicit);
    }

    #[test]
    fn manifest_may_omit_the_file_when_unambiguous() {
        let dir = tempdir();
        let pkg = dir.join("solo");
        touch(&pkg.join("only.webm"));
        std::fs::write(pkg.join(MANIFEST_NAME), "[wallpaper]\ntitle = \"Solo\"\n").unwrap();
        let library = Library::load_from(&dir).unwrap();
        assert_eq!(library.get("solo").unwrap().source, pkg.join("only.webm"));
    }

    #[test]
    fn ambiguous_package_is_reported_not_guessed() {
        let dir = tempdir();
        let pkg = dir.join("two");
        touch(&pkg.join("a.mp4"));
        touch(&pkg.join("b.mp4"));
        std::fs::write(pkg.join(MANIFEST_NAME), "[wallpaper]\n").unwrap();
        let library = Library::load_from(&dir).unwrap();
        assert!(library.wallpapers.is_empty());
        assert_eq!(library.errors.len(), 1);
        assert!(library.errors[0].1.contains("2 video files"));
    }

    #[test]
    fn directory_without_manifest_still_works() {
        let dir = tempdir();
        touch(&dir.join("pack/clip.mp4"));
        let library = Library::load_from(&dir).unwrap();
        assert_eq!(library.wallpapers.len(), 1);
        assert_eq!(library.wallpapers[0].id, "clip");
    }

    #[test]
    fn duplicate_ids_are_suffixed_not_dropped() {
        let dir = tempdir();
        touch(&dir.join("clip.mp4"));
        touch(&dir.join("pack/clip.mp4"));
        let library = Library::load_from(&dir).unwrap();
        assert_eq!(library.wallpapers.len(), 2);
        let ids: Vec<_> = library.wallpapers.iter().map(|wp| wp.id.as_str()).collect();
        assert!(ids.contains(&"clip"), "{ids:?}");
        assert!(ids.contains(&"clip-2"), "{ids:?}");
    }

    #[test]
    fn missing_library_dir_is_empty_not_an_error() {
        let library = Library::load_from(Path::new("/nonexistent/wallflux")).unwrap();
        assert!(library.wallpapers.is_empty());
    }
}
