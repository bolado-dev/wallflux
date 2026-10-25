//! Importing from an existing Wallpaper Engine installation.
//!
//! Wallpaper Engine stores each subscribed item as a Steam Workshop folder
//! with a `project.json` manifest:
//!
//! ```text
//! ~/.steam/steam/steamapps/workshop/content/431960/1234567890/
//! ├── project.json
//! ├── preview.jpg
//! └── scene.mp4
//! ```
//!
//! What wallflux can and cannot do with those, honestly:
//!
//! | `type`  | Supported | Why |
//! |---------|-----------|-----|
//! | `video` | **Yes**   | It is an ordinary mp4/webm next to the manifest |
//! | `scene` | No        | A proprietary binary `.pkg`; there is nothing to implement against |
//! | `web`   | Not yet   | HTML/JS, possible later with a webview surface |
//! | `application` | No  | An executable, and not something to run from a wallpaper tool |
//!
//! This only ever reads files already on the user's own disk and copies them
//! into their own library. It does not download anything, and nothing here
//! redistributes a wallpaper to anyone.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::library::{VIDEO_EXTENSIONS, slugify};
use crate::paths;

/// Wallpaper Engine's Steam app id. Its workshop content lives under this.
pub const STEAM_APP_ID: &str = "431960";

const MANIFEST: &str = "project.json";

/// What a workshop item turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// A plain video file: importable.
    Video,
    /// A `.pkg` scene. Proprietary format.
    Scene,
    /// An HTML/JS wallpaper. Not supported yet.
    Web,
    /// An executable.
    Application,
    /// A `type` this build does not recognise.
    Unknown,
}

impl ItemKind {
    fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "video" => ItemKind::Video,
            "scene" => ItemKind::Scene,
            "web" => ItemKind::Web,
            "application" => ItemKind::Application,
            _ => ItemKind::Unknown,
        }
    }

    pub fn importable(self) -> bool {
        self == ItemKind::Video
    }

    /// One line for a user asking why something was skipped.
    pub fn reason(self) -> &'static str {
        match self {
            ItemKind::Video => "importable",
            ItemKind::Scene => "scene wallpapers use a proprietary .pkg format",
            ItemKind::Web => "web wallpapers are not supported yet",
            ItemKind::Application => "application wallpapers are executables",
            ItemKind::Unknown => "unrecognised wallpaper type",
        }
    }
}

/// The subset of `project.json` wallflux reads. Everything else is ignored:
/// the file carries a lot of editor state that means nothing here.
#[derive(Debug, Deserialize)]
struct ProjectJson {
    #[serde(default)]
    title: Option<String>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    preview: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    contentrating: Option<String>,
}

/// A workshop item found on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// The workshop id, which is the folder name.
    pub workshop_id: String,
    pub title: String,
    pub kind: ItemKind,
    /// The video file, when there is one.
    pub source: Option<PathBuf>,
    pub preview: Option<PathBuf>,
    pub tags: Vec<String>,
    /// Wallpaper Engine's own rating, e.g. `Everyone` or `Mature`.
    pub content_rating: Option<String>,
    pub root: PathBuf,
}

impl Item {
    /// The id this would get in the wallflux library.
    pub fn wallflux_id(&self) -> String {
        let slug = slugify(&self.title);
        if slug.is_empty() {
            format!("we-{}", self.workshop_id)
        } else {
            slug
        }
    }
}

/// Places Wallpaper Engine's workshop content is normally found.
///
/// `$WALLFLUX_WORKSHOP_DIR` overrides the lot, which is also how the tests
/// run against a fixture.
pub fn workshop_dirs() -> Vec<PathBuf> {
    if let Some(dir) = std::env::var_os("WALLFLUX_WORKSHOP_DIR") {
        return vec![PathBuf::from(dir)];
    }
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    [
        ".steam/steam/steamapps/workshop/content",
        ".local/share/Steam/steamapps/workshop/content",
        ".steam/root/steamapps/workshop/content",
        ".var/app/com.valvesoftware.Steam/data/Steam/steamapps/workshop/content",
    ]
    .iter()
    .map(|relative| home.join(relative).join(STEAM_APP_ID))
    .filter(|path| path.is_dir())
    .collect()
}

/// Every workshop item under `dir`, importable or not.
///
/// Unreadable items are reported as `Unknown` rather than dropped: a library
/// that silently lists 30 of your 50 wallpapers is worse than one that says
/// why the other 20 are missing.
pub fn scan(dir: &Path) -> Result<Vec<Item>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("cannot read {}", dir.display())),
    };

    let mut items = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        match read_item(&path) {
            Ok(Some(item)) => items.push(item),
            Ok(None) => {}
            Err(err) => tracing::debug!(path = %path.display(), "skipping: {err:#}"),
        }
    }
    items.sort_by(|a, b| a.title.cmp(&b.title));
    Ok(items)
}

/// Scans every known location.
pub fn scan_all() -> Result<Vec<Item>> {
    let mut items = Vec::new();
    for dir in workshop_dirs() {
        items.extend(scan(&dir)?);
    }
    Ok(items)
}

fn read_item(dir: &Path) -> Result<Option<Item>> {
    let manifest_path = dir.join(MANIFEST);
    if !manifest_path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("cannot read {}", manifest_path.display()))?;
    let project: ProjectJson = serde_json::from_str(&text)
        .with_context(|| format!("invalid JSON in {}", manifest_path.display()))?;

    let workshop_id = dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown")
        .to_string();
    let kind = project
        .kind
        .as_deref()
        .map(ItemKind::parse)
        .unwrap_or(ItemKind::Unknown);

    // `file` is relative to the item directory. Trust it only when it is
    // actually a video; a scene's `file` points at a .pkg.
    let source = project
        .file
        .as_deref()
        .map(|file| dir.join(file))
        .filter(|path| path.is_file() && has_video_extension(path));

    Ok(Some(Item {
        title: project
            .title
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| format!("Workshop item {workshop_id}")),
        preview: project
            .preview
            .as_deref()
            .map(|preview| dir.join(preview))
            .filter(|path| path.is_file()),
        tags: project.tags,
        content_rating: project.contentrating,
        kind,
        source,
        workshop_id,
        root: dir.to_path_buf(),
    }))
}

fn has_video_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| VIDEO_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// Copies a video item into the wallflux library as its own package.
///
/// Copies rather than symlinks: unsubscribing in Steam deletes the workshop
/// folder, and a library full of dangling links is a worse surprise than
/// using the disk space.
pub fn import(item: &Item, library_root: &Path) -> Result<String> {
    if !item.kind.importable() {
        bail!("{}: {}", item.title, item.kind.reason());
    }
    let source = item
        .source
        .as_ref()
        .with_context(|| format!("{}: the manifest names no video file", item.title))?;

    paths::ensure_dir(library_root)?;
    let id = unique_id(library_root, &item.wallflux_id());
    let package = library_root.join(&id);
    std::fs::create_dir(&package)
        .with_context(|| format!("cannot create {}", package.display()))?;

    let extension = source
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("mp4")
        .to_ascii_lowercase();
    let destination = package.join(format!("video.{extension}"));
    std::fs::copy(source, &destination).with_context(|| {
        format!(
            "cannot copy {} into {}",
            source.display(),
            destination.display()
        )
    })?;

    if let Some(preview) = &item.preview
        && let Some(ext) = preview.extension().and_then(|ext| ext.to_str())
    {
        // A preview that fails to copy is not worth failing the import over;
        // one gets generated from the video instead.
        let _ = std::fs::copy(preview, package.join(format!("preview.{ext}")));
    }

    let mut manifest = String::from("[wallpaper]\n");
    manifest.push_str(&format!("title = {:?}\n", item.title));
    manifest.push_str("kind = \"video\"\n");
    if !item.tags.is_empty() {
        let tags: Vec<String> = item.tags.iter().map(|tag| format!("{tag:?}")).collect();
        manifest.push_str(&format!("tags = [{}]\n", tags.join(", ")));
    }
    manifest.push_str(&format!(
        "\n# Imported from Wallpaper Engine workshop item {}\n",
        item.workshop_id
    ));
    manifest.push_str(&format!("\n[video]\nfile = \"video.{extension}\"\n"));
    std::fs::write(package.join("wallflux.toml"), manifest)
        .with_context(|| format!("cannot write the manifest in {}", package.display()))?;

    Ok(id)
}

fn unique_id(root: &Path, base: &str) -> String {
    let base = if base.is_empty() { "wallpaper" } else { base };
    if !root.join(base).exists() {
        return base.to_string();
    }
    for suffix in 2..1000 {
        let candidate = format!("{base}-{suffix}");
        if !root.join(&candidate).exists() {
            return candidate;
        }
    }
    format!("{base}-{}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("wallflux-we-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// Builds a workshop item the way Wallpaper Engine lays one out.
    fn workshop_item(root: &Path, id: &str, project: &str, files: &[&str]) -> PathBuf {
        let dir = root.join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MANIFEST), project).unwrap();
        for file in files {
            std::fs::write(dir.join(file), b"content").unwrap();
        }
        dir
    }

    #[test]
    fn a_video_item_is_read_and_importable() {
        let root = fixture("video");
        workshop_item(
            &root,
            "1234567890",
            r#"{"title":"Neon Drive","type":"video","file":"scene.mp4",
                "preview":"preview.jpg","tags":["Sci-Fi","Relaxing"],
                "contentrating":"Everyone"}"#,
            &["scene.mp4", "preview.jpg"],
        );
        let items = scan(&root).unwrap();
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.title, "Neon Drive");
        assert_eq!(item.kind, ItemKind::Video);
        assert!(item.kind.importable());
        assert!(item.source.is_some());
        assert!(item.preview.is_some());
        assert_eq!(item.tags, vec!["Sci-Fi", "Relaxing"]);
        assert_eq!(item.content_rating.as_deref(), Some("Everyone"));
        assert_eq!(item.wallflux_id(), "neon-drive");
    }

    #[test]
    fn a_scene_item_is_listed_but_refused() {
        let root = fixture("scene");
        workshop_item(
            &root,
            "222",
            r#"{"title":"Fancy Scene","type":"scene","file":"scene.pkg"}"#,
            &["scene.pkg"],
        );
        let items = scan(&root).unwrap();
        assert_eq!(items[0].kind, ItemKind::Scene);
        assert!(!items[0].kind.importable());
        // The .pkg must not be mistaken for a video just because it is named
        // in `file`.
        assert_eq!(items[0].source, None);

        let library = fixture("scene-library");
        let err = import(&items[0], &library).unwrap_err().to_string();
        assert!(err.contains("proprietary"), "{err}");
    }

    #[test]
    fn the_type_field_is_matched_case_insensitively() {
        let root = fixture("case");
        workshop_item(
            &root,
            "333",
            r#"{"title":"Shouty","type":"Video","file":"a.mp4"}"#,
            &["a.mp4"],
        );
        assert_eq!(scan(&root).unwrap()[0].kind, ItemKind::Video);
    }

    #[test]
    fn web_and_application_items_are_named_not_hidden() {
        let root = fixture("other");
        workshop_item(&root, "444", r#"{"title":"Webby","type":"web"}"#, &[]);
        workshop_item(
            &root,
            "555",
            r#"{"title":"Appy","type":"application"}"#,
            &[],
        );
        let kinds: Vec<_> = scan(&root).unwrap().iter().map(|i| i.kind).collect();
        assert!(kinds.contains(&ItemKind::Web));
        assert!(kinds.contains(&ItemKind::Application));
    }

    #[test]
    fn a_folder_without_a_manifest_is_skipped() {
        let root = fixture("bare");
        std::fs::create_dir_all(root.join("666")).unwrap();
        assert!(scan(&root).unwrap().is_empty());
    }

    #[test]
    fn importing_copies_the_video_the_preview_and_writes_a_manifest() {
        let root = fixture("import");
        workshop_item(
            &root,
            "777",
            r#"{"title":"Neon Drive","type":"video","file":"scene.mp4",
                "preview":"preview.jpg","tags":["Sci-Fi"]}"#,
            &["scene.mp4", "preview.jpg"],
        );
        let items = scan(&root).unwrap();
        let library = fixture("import-library");
        let id = import(&items[0], &library).unwrap();
        assert_eq!(id, "neon-drive");

        let package = library.join(&id);
        assert!(package.join("video.mp4").is_file());
        assert!(package.join("preview.jpg").is_file());
        let manifest = std::fs::read_to_string(package.join("wallflux.toml")).unwrap();
        assert!(manifest.contains("title = \"Neon Drive\""));
        assert!(manifest.contains("tags = [\"Sci-Fi\"]"));
        assert!(manifest.contains("workshop item 777"));

        // The result has to be a package the daemon can actually read back.
        let found = crate::library::Library::load_from(&library).unwrap();
        assert!(found.errors.is_empty(), "{:?}", found.errors);
        assert_eq!(found.get("neon-drive").unwrap().title, "Neon Drive");
    }

    #[test]
    fn importing_the_same_item_twice_does_not_overwrite() {
        let root = fixture("twice");
        workshop_item(
            &root,
            "888",
            r#"{"title":"Twice","type":"video","file":"a.mp4"}"#,
            &["a.mp4"],
        );
        let items = scan(&root).unwrap();
        let library = fixture("twice-library");
        assert_eq!(import(&items[0], &library).unwrap(), "twice");
        assert_eq!(import(&items[0], &library).unwrap(), "twice-2");
    }

    #[test]
    fn an_untitled_item_falls_back_to_its_workshop_id() {
        let root = fixture("untitled");
        workshop_item(
            &root,
            "999",
            r#"{"type":"video","file":"a.mp4"}"#,
            &["a.mp4"],
        );
        let items = scan(&root).unwrap();
        assert_eq!(items[0].title, "Workshop item 999");
        assert_eq!(items[0].wallflux_id(), "workshop-item-999");
    }
}
