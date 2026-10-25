//! The online gallery: browse and download freely licensed wallpapers.
//!
//! Two sources, both optional and neither needing an account to get started:
//!
//! * **The curated catalogue.** A JSON file versioned in the wallflux
//!   repository listing hand-picked wallpapers under permissive licences.
//!   It needs no API key, has no rate limit, and anyone can add to it with a
//!   pull request. This is what makes the gallery work on first launch.
//! * **Pexels**, when the user supplies their own free API key. Thousands of
//!   videos and a real search, at the cost of signing up — so it is strictly
//!   an upgrade, never a requirement.
//!
//! Wallpaper Engine's own workshop is deliberately **not** a source. Its
//! catalogue is paid, account-gated content; the only ways to fetch it
//! without owning it work by circumventing that gate.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::library::slugify;
use crate::paths;

/// Where the curated catalogue lives. Overridable so a fork, or a test, can
/// point somewhere else.
pub const DEFAULT_CATALOG_URL: &str =
    "https://raw.githubusercontent.com/bolado-dev/wallflux/main/catalog/catalog.json";

/// Generous: these are video files over whatever connection the user has.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);
const METADATA_TIMEOUT: Duration = Duration::from_secs(20);

/// Refuse anything absurd rather than filling someone's disk silently.
const MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;

/// Which source an entry came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Curated,
    Pexels,
}

/// One downloadable wallpaper.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GalleryItem {
    /// Unique within its source.
    pub id: String,
    pub title: String,
    pub author: Option<String>,
    /// SPDX identifier where there is one, else the licence's name.
    pub license: String,
    /// The page a human should visit to check the licence and credit the
    /// author. Never the raw file.
    pub source_url: Option<String>,
    /// Direct link to the video.
    pub video_url: String,
    /// Still image for the grid.
    pub preview_url: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_secs: Option<f32>,
    pub source: Source,
}

impl GalleryItem {
    /// The id this takes in the local library.
    pub fn local_id(&self) -> String {
        let slug = slugify(&self.title);
        if slug.is_empty() {
            format!("{}-{}", source_prefix(self.source), self.id)
        } else {
            slug
        }
    }
}

fn source_prefix(source: Source) -> &'static str {
    match source {
        Source::Curated => "wallflux",
        Source::Pexels => "pexels",
    }
}

/// The catalogue file's on-disk shape. Versioned so an older wallflux can
/// refuse a newer catalogue instead of misreading it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Catalog {
    pub version: u32,
    #[serde(default)]
    pub items: Vec<GalleryItem>,
}

/// Highest catalogue version this build understands.
pub const CATALOG_VERSION: u32 = 1;

/// Fetches and parses the curated catalogue.
pub fn fetch_catalog(url: &str) -> Result<Vec<GalleryItem>> {
    let text = get_text(url).with_context(|| format!("no se puede leer el catálogo en {url}"))?;
    parse_catalog(&text)
}

/// Split out from the fetch so the parsing rules can be tested without a
/// network, which is most of what can actually go wrong here.
pub fn parse_catalog(text: &str) -> Result<Vec<GalleryItem>> {
    let catalog: Catalog = serde_json::from_str(text).context("el catálogo no es JSON válido")?;
    if catalog.version > CATALOG_VERSION {
        bail!(
            "el catálogo es de la versión {} y esta versión de wallflux entiende hasta la {CATALOG_VERSION}; \
             actualiza wallflux",
            catalog.version
        );
    }
    let mut items = Vec::with_capacity(catalog.items.len());
    for mut item in catalog.items {
        if item.video_url.is_empty() {
            continue;
        }
        if !is_http_url(&item.video_url) {
            // A catalogue is fetched from the network; a file:// or data:
            // entry in it would be a way to make the app read local paths it
            // was never pointed at.
            tracing::warn!(url = %item.video_url, "entrada del catálogo con URL no http(s), ignorada");
            continue;
        }
        item.source = Source::Curated;
        items.push(item);
    }
    Ok(items)
}

fn is_http_url(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

/// Searches Pexels. `query` empty means their curated popular videos.
pub fn search_pexels(api_key: &str, query: &str, per_page: u32) -> Result<Vec<GalleryItem>> {
    if api_key.trim().is_empty() {
        bail!("falta la clave de la API de Pexels");
    }
    let per_page = per_page.clamp(1, 80);
    let url = if query.trim().is_empty() {
        format!("https://api.pexels.com/videos/popular?per_page={per_page}")
    } else {
        format!(
            "https://api.pexels.com/videos/search?per_page={per_page}&query={}",
            urlencode(query)
        )
    };

    let mut response = ureq::get(&url)
        .header("Authorization", api_key.trim())
        .config()
        .timeout_global(Some(METADATA_TIMEOUT))
        .build()
        .call()
        .context("la búsqueda en Pexels ha fallado")?;
    let payload: PexelsResponse = response
        .body_mut()
        .read_json()
        .context("Pexels ha devuelto algo que no se puede interpretar")?;

    Ok(payload.videos.into_iter().filter_map(pexels_item).collect())
}

#[derive(Debug, Deserialize)]
struct PexelsResponse {
    #[serde(default)]
    videos: Vec<PexelsVideo>,
}

#[derive(Debug, Deserialize)]
struct PexelsVideo {
    id: u64,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
    #[serde(default)]
    duration: Option<f32>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    image: Option<String>,
    #[serde(default)]
    user: Option<PexelsUser>,
    #[serde(default)]
    video_files: Vec<PexelsFile>,
}

#[derive(Debug, Deserialize)]
struct PexelsUser {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PexelsFile {
    #[serde(default)]
    link: Option<String>,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    file_type: Option<String>,
}

fn pexels_item(video: PexelsVideo) -> Option<GalleryItem> {
    // Pexels returns several renditions. Pick the largest that is not
    // gigantic: a wallpaper gains nothing from 4K that a 1080p loop does not
    // already give, and costs four times the decode.
    let file = video
        .video_files
        .iter()
        .filter(|file| {
            file.link.is_some()
                && file
                    .file_type
                    .as_deref()
                    .map(|kind| kind.starts_with("video/"))
                    .unwrap_or(true)
        })
        .filter(|file| file.width.unwrap_or(0) <= 2560)
        .max_by_key(|file| file.width.unwrap_or(0))
        .or_else(|| video.video_files.first())?;

    Some(GalleryItem {
        title: format!("Pexels {}", video.id),
        id: video.id.to_string(),
        author: video.user.and_then(|user| user.name),
        license: "Pexels License".to_string(),
        source_url: video.url,
        video_url: file.link.clone()?,
        preview_url: video.image,
        tags: Vec::new(),
        width: file.width.or(video.width),
        height: video.height,
        duration_secs: video.duration,
        source: Source::Pexels,
    })
}

/// Percent-encodes a query string value. Only the handful of characters a
/// search term realistically contains; this is not a general URL encoder.
fn urlencode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            b' ' => out.push_str("%20"),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn get_text(url: &str) -> Result<String> {
    let mut response = ureq::get(url)
        .config()
        .timeout_global(Some(METADATA_TIMEOUT))
        .build()
        .call()?;
    Ok(response.body_mut().read_to_string()?)
}

/// Progress of a download, in bytes. `total` is `None` when the server does
/// not say how big the file is.
///
/// Always passed, never optional: a caller that does not care passes
/// [`no_progress`], which keeps every signature here free of the lifetime
/// gymnastics that an `Option<&mut dyn FnMut>` needs to be reborrowed.
pub type ProgressFn<'a> = &'a mut dyn FnMut(u64, Option<u64>);

/// A progress callback that does nothing.
pub fn no_progress() -> impl FnMut(u64, Option<u64>) {
    |_, _| {}
}

/// Downloads an item into the library as its own package, and returns its
/// local id.
///
/// Writes to a temporary directory and moves it into place at the end, so an
/// interrupted download never leaves a half-written package that the daemon
/// would then try to play.
pub fn download(
    item: &GalleryItem,
    library_root: &Path,
    progress: ProgressFn<'_>,
) -> Result<String> {
    if !is_http_url(&item.video_url) {
        bail!("la URL del vídeo no es http(s)");
    }
    paths::ensure_dir(library_root)?;
    let id = unique_id(library_root, &item.local_id());
    let package = library_root.join(&id);
    let staging = library_root.join(format!(".{id}.part"));
    let _ = std::fs::remove_dir_all(&staging);
    paths::ensure_dir(&staging)?;

    // A plain call rather than a closure: the progress callback is a
    // borrowed `&mut dyn FnMut`, and moving it into a closure makes the
    // borrow outlive what the compiler can see.
    match fill_staging(item, &staging, progress) {
        Ok(()) => {}
        Err(err) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(err);
        }
    }

    std::fs::rename(&staging, &package).with_context(|| {
        format!(
            "no se ha podido mover {} a {}",
            staging.display(),
            package.display()
        )
    })?;
    Ok(id)
}

/// Downloads the video, its preview and writes the manifest into `staging`.
fn fill_staging(item: &GalleryItem, staging: &Path, progress: ProgressFn<'_>) -> Result<()> {
    let extension = guess_extension(&item.video_url);
    let video_path = staging.join(format!("video.{extension}"));
    download_to(&item.video_url, &video_path, progress)
        .with_context(|| format!("no se ha podido descargar {}", item.video_url))?;

    if let Some(preview_url) = &item.preview_url
        && is_http_url(preview_url)
    {
        let preview_extension = guess_extension(preview_url);
        let preview_path = staging.join(format!("preview.{preview_extension}"));
        // A missing preview is cosmetic — one gets generated from the video
        // — so this must never fail the download.
        if let Err(err) = download_to(preview_url, &preview_path, &mut no_progress()) {
            tracing::debug!(%err, "no se ha podido descargar la vista previa");
        }
    }

    std::fs::write(
        staging.join("wallflux.toml"),
        manifest_for(item, &extension),
    )
    .context("no se ha podido escribir el manifiesto")?;
    Ok(())
}

fn manifest_for(item: &GalleryItem, extension: &str) -> String {
    let mut manifest = String::from("[wallpaper]\n");
    manifest.push_str(&format!("title = {:?}\n", item.title));
    if let Some(author) = &item.author {
        manifest.push_str(&format!("author = {author:?}\n"));
    }
    manifest.push_str(&format!("license = {:?}\n", item.license));
    manifest.push_str("kind = \"video\"\n");
    if !item.tags.is_empty() {
        let tags: Vec<String> = item.tags.iter().map(|tag| format!("{tag:?}")).collect();
        manifest.push_str(&format!("tags = [{}]\n", tags.join(", ")));
    }
    // The origin goes in a comment rather than a field: it is provenance for
    // a human checking the licence, not something the daemon reads.
    if let Some(url) = &item.source_url {
        manifest.push_str(&format!("\n# Descargado de {url}\n"));
    }
    manifest.push_str(&format!("\n[video]\nfile = \"video.{extension}\"\n"));
    manifest
}

fn download_to(url: &str, destination: &Path, progress: ProgressFn<'_>) -> Result<()> {
    use std::io::{Read, Write};

    let mut response = ureq::get(url)
        .config()
        .timeout_global(Some(DOWNLOAD_TIMEOUT))
        .build()
        .call()?;

    let total = response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if let Some(total) = total
        && total > MAX_DOWNLOAD_BYTES
    {
        bail!(
            "el archivo son {} MB, más del límite de {} MB",
            total / 1_048_576,
            MAX_DOWNLOAD_BYTES / 1_048_576
        );
    }

    let mut reader = response.body_mut().as_reader();
    let mut file = std::fs::File::create(destination)
        .with_context(|| format!("no se puede crear {}", destination.display()))?;
    let mut buffer = vec![0u8; 64 * 1024];
    let mut written: u64 = 0;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        written += read as u64;
        // Also enforced while streaming: a server can lie about, or omit,
        // Content-Length.
        if written > MAX_DOWNLOAD_BYTES {
            bail!(
                "la descarga supera el límite de {} MB",
                MAX_DOWNLOAD_BYTES / 1_048_576
            );
        }
        file.write_all(&buffer[..read])?;
        progress(written, total);
    }
    file.flush()?;
    Ok(())
}

/// Extension from a URL path, falling back to mp4. Ignores query strings,
/// which Pexels links always carry.
fn guess_extension(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let candidate = path
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, extension)| extension.to_ascii_lowercase());
    match candidate {
        Some(extension)
            if extension.len() <= 5 && extension.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            extension
        }
        _ => "mp4".to_string(),
    }
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

/// Local library directory, for callers that do not want to reach for
/// [`paths`] themselves.
pub fn library_root() -> Result<PathBuf> {
    paths::library_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item() -> GalleryItem {
        GalleryItem {
            id: "1".into(),
            title: "Neon Drive".into(),
            author: Some("Alguien".into()),
            license: "CC0-1.0".into(),
            source_url: Some("https://example.org/neon".into()),
            video_url: "https://example.org/neon.mp4".into(),
            preview_url: None,
            tags: vec!["abstracto".into()],
            width: Some(1920),
            height: Some(1080),
            duration_secs: Some(12.0),
            source: Source::Curated,
        }
    }

    #[test]
    fn a_catalog_round_trips() {
        let catalog = Catalog {
            version: CATALOG_VERSION,
            items: vec![item()],
        };
        let text = serde_json::to_string_pretty(&catalog).unwrap();
        let parsed = parse_catalog(&text).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].title, "Neon Drive");
    }

    #[test]
    fn a_newer_catalog_is_refused_rather_than_misread() {
        let text = format!(r#"{{"version": {}, "items": []}}"#, CATALOG_VERSION + 1);
        let err = parse_catalog(&text).unwrap_err().to_string();
        assert!(err.contains("actualiza wallflux"), "{err}");
    }

    #[test]
    fn non_http_entries_are_dropped() {
        // A catalogue comes off the network, so an entry naming a local path
        // would be a way to make wallflux read somewhere it was never sent.
        let text = r#"{"version":1,"items":[
            {"id":"a","title":"Local","license":"CC0-1.0",
             "video_url":"file:///etc/passwd","source":"curated"},
            {"id":"b","title":"Remota","license":"CC0-1.0",
             "video_url":"https://example.org/b.mp4","source":"curated"}
        ]}"#;
        let items = parse_catalog(text).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "b");
    }

    #[test]
    fn the_source_is_forced_even_if_the_catalog_lies() {
        let text = r#"{"version":1,"items":[
            {"id":"a","title":"X","license":"CC0-1.0",
             "video_url":"https://example.org/a.mp4","source":"pexels"}
        ]}"#;
        assert_eq!(parse_catalog(text).unwrap()[0].source, Source::Curated);
    }

    #[test]
    fn extensions_survive_query_strings() {
        assert_eq!(guess_extension("https://x.org/a.webm"), "webm");
        assert_eq!(
            guess_extension("https://player.vimeo.com/external/123.mp4?s=abc&profile_id=175"),
            "mp4"
        );
        // No extension, or something that is clearly not one.
        assert_eq!(guess_extension("https://x.org/video"), "mp4");
        assert_eq!(guess_extension("https://x.org/a.verylongthing"), "mp4");
    }

    #[test]
    fn local_ids_fall_back_when_a_title_has_no_letters() {
        let mut quiet = item();
        quiet.title = "!!!".into();
        assert_eq!(quiet.local_id(), "wallflux-1");
        assert_eq!(item().local_id(), "neon-drive");
    }

    #[test]
    fn the_manifest_is_a_package_the_daemon_can_read() {
        let dir = std::env::temp_dir().join(format!("wallflux-gal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let package = dir.join("neon-drive");
        std::fs::create_dir_all(&package).unwrap();
        std::fs::write(package.join("video.mp4"), b"x").unwrap();
        std::fs::write(package.join("wallflux.toml"), manifest_for(&item(), "mp4")).unwrap();

        let library = crate::library::Library::load_from(&dir).unwrap();
        assert!(library.errors.is_empty(), "{:?}", library.errors);
        let wallpaper = library.get("neon-drive").unwrap();
        assert_eq!(wallpaper.author.as_deref(), Some("Alguien"));
        assert_eq!(wallpaper.license.as_deref(), Some("CC0-1.0"));
    }

    #[test]
    fn urlencode_escapes_what_a_search_box_produces() {
        assert_eq!(urlencode("neon city"), "neon%20city");
        assert_eq!(urlencode("café"), "caf%C3%A9");
        assert_eq!(urlencode("a-b_c.d~e"), "a-b_c.d~e");
    }

    #[test]
    fn pexels_prefers_the_largest_sane_rendition() {
        let video = PexelsVideo {
            id: 42,
            width: Some(3840),
            height: Some(2160),
            duration: Some(10.0),
            url: Some("https://pexels.com/video/42".into()),
            image: Some("https://pexels.com/42.jpg".into()),
            user: Some(PexelsUser {
                name: Some("Autora".into()),
            }),
            video_files: vec![
                PexelsFile {
                    link: Some("https://f/sd.mp4".into()),
                    width: Some(640),
                    file_type: Some("video/mp4".into()),
                },
                PexelsFile {
                    link: Some("https://f/hd.mp4".into()),
                    width: Some(1920),
                    file_type: Some("video/mp4".into()),
                },
                // 4K: skipped on purpose, it costs four times the decode for
                // no visible gain as a background.
                PexelsFile {
                    link: Some("https://f/4k.mp4".into()),
                    width: Some(3840),
                    file_type: Some("video/mp4".into()),
                },
            ],
        };
        let item = pexels_item(video).unwrap();
        assert_eq!(item.video_url, "https://f/hd.mp4");
        assert_eq!(item.author.as_deref(), Some("Autora"));
        assert_eq!(item.source, Source::Pexels);
    }
}
