//! Preview images and importing.
//!
//! A wallpaper package may ship its own `preview.jpg`. When it does not, one
//! frame is pulled out with ffmpeg and cached, keyed by the source file's path,
//! size and mtime — so re-encoding a video invalidates its thumbnail without
//! anyone having to remember to clear a cache.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use wallflux_core::library::{Library, VIDEO_EXTENSIONS, slugify};
use wallflux_core::paths;

/// Where in the clip to grab the frame. Not zero: plenty of videos open on a
/// black fade, which makes for a useless thumbnail.
const SEEK_SECONDS: f32 = 1.0;

/// Width of the generated thumbnail. Height follows the aspect ratio.
const THUMB_WIDTH: u32 = 640;

/// Returns a still image for `id`, generating and caching one if the package
/// does not ship a preview.
pub fn thumbnail_for(id: &str) -> Result<PathBuf> {
    let library = Library::load()?;
    let wallpaper = library
        .get(id)
        .with_context(|| format!("no wallpaper with id {id:?}"))?;

    if let Some(preview) = &wallpaper.preview {
        return Ok(preview.clone());
    }

    let cache = paths::cache_dir()?.join("thumbnails");
    paths::ensure_dir(&cache)?;
    let target = cache.join(format!("{}.jpg", cache_key(&wallpaper.source)?));
    if target.is_file() {
        return Ok(target);
    }

    extract_frame(&wallpaper.source, &target)?;
    Ok(target)
}

/// A name that changes whenever the source does. Hashing the bytes would be
/// more precise and would mean reading a 200 MB file to draw a grid.
fn cache_key(source: &Path) -> Result<String> {
    let metadata = std::fs::metadata(source)
        .with_context(|| format!("cannot stat {}", source.display()))?;
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or(0);

    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in source
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .copied()
        .chain(metadata.len().to_le_bytes())
        .chain(modified.to_le_bytes())
    {
        // FNV-1a: not cryptographic, and does not need to be — this names a
        // cache file, it does not protect anything.
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    Ok(format!("{hash:016x}"))
}

fn extract_frame(source: &Path, target: &Path) -> Result<()> {
    // Write to a temporary name and rename into place, so two GUI windows
    // racing on the same wallpaper cannot leave a half-written JPEG behind.
    //
    // The temporary keeps the .jpg extension on the end: ffmpeg picks its
    // output format from the extension, and a file ending in .part makes it
    // fail with "Unable to choose an output format".
    let temporary = target.with_extension("part.jpg");
    let output = Command::new("ffmpeg")
        .arg("-nostdin")
        .arg("-loglevel")
        .arg("error")
        .arg("-ss")
        .arg(SEEK_SECONDS.to_string())
        .arg("-i")
        .arg(source)
        .arg("-frames:v")
        .arg("1")
        .arg("-vf")
        .arg(format!("scale={THUMB_WIDTH}:-2"))
        // Explicit, so the output format never depends on the file name.
        .arg("-f")
        .arg("image2")
        .arg("-y")
        .arg(&temporary)
        .output()
        .context(
            "cannot run ffmpeg — install it to get thumbnails \
             (Debian/Kali: `sudo apt install ffmpeg`)",
        )?;

    if !output.status.success() || !temporary.is_file() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "ffmpeg could not read a frame from {}: {}",
            source.display(),
            stderr.trim()
        );
    }
    std::fs::rename(&temporary, target)
        .with_context(|| format!("cannot move the thumbnail into {}", target.display()))?;
    Ok(())
}

/// Copies `source` into the library as its own package, with a manifest.
/// Returns the new wallpaper id.
pub fn import(source: &Path, title: Option<&str>) -> Result<String> {
    if !source.is_file() {
        bail!("{} is not a file", source.display());
    }
    let extension = source
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !VIDEO_EXTENSIONS.contains(&extension.as_str()) {
        bail!(
            "{} is not a supported video file (expected one of: {})",
            source.display(),
            VIDEO_EXTENSIONS.join(", ")
        );
    }

    let stem = source
        .file_stem()
        .and_then(|name| name.to_str())
        .context("the file name is not valid UTF-8")?;
    let title = title.unwrap_or(stem).to_string();

    let root = paths::library_dir()?;
    paths::ensure_dir(&root)?;
    let id = unique_id(&root, &slugify(stem));
    let package = root.join(&id);
    std::fs::create_dir(&package)
        .with_context(|| format!("cannot create {}", package.display()))?;

    let destination = package.join(format!("video.{extension}"));
    std::fs::copy(source, &destination).with_context(|| {
        format!(
            "cannot copy {} into {}",
            source.display(),
            destination.display()
        )
    })?;

    let manifest = format!(
        "[wallpaper]\n\
         title = {title:?}\n\
         kind = \"video\"\n\
         \n\
         [video]\n\
         file = \"video.{extension}\"\n"
    );
    std::fs::write(package.join("wallflux.toml"), manifest)
        .with_context(|| format!("cannot write the manifest in {}", package.display()))?;

    Ok(id)
}

/// Appends `-2`, `-3`… until the directory name is free, so importing the same
/// file twice does not overwrite the first copy.
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

    #[test]
    fn imports_use_the_daemons_own_slug_rules() {
        // Shared with wallflux-core rather than reimplemented: an id the GUI
        // invents has to be one the daemon can look up.
        assert_eq!(slugify("Neon City 02.mp4"), "neon-city-02-mp4");
    }

    #[test]
    fn an_empty_slug_falls_back_to_a_usable_name() {
        let root = std::env::temp_dir().join(format!("wallflux-import-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        assert_eq!(unique_id(&root, ""), "wallpaper");
    }

    #[test]
    fn importing_twice_does_not_collide() {
        let root = std::env::temp_dir().join(format!("wallflux-unique-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("clip")).unwrap();
        assert_eq!(unique_id(&root, "clip"), "clip-2");
    }

    #[test]
    fn the_cache_key_changes_with_the_file() {
        let dir = std::env::temp_dir().join(format!("wallflux-key-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.mp4");
        std::fs::write(&path, b"one").unwrap();
        let first = cache_key(&path).unwrap();
        std::fs::write(&path, b"a different length").unwrap();
        let second = cache_key(&path).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn a_non_video_is_refused() {
        let dir = std::env::temp_dir().join(format!("wallflux-reject-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notes.txt");
        std::fs::write(&path, b"hello").unwrap();
        let err = import(&path, None).unwrap_err().to_string();
        assert!(err.contains("not a supported video"), "{err}");
    }
    /// Generates a tiny real video. Returns None when ffmpeg is unavailable,
    /// so the suite still passes on a machine without it.
    fn sample_video(dir: &Path) -> Option<PathBuf> {
        let path = dir.join("sample.mp4");
        let status = Command::new("ffmpeg")
            .args([
                "-nostdin", "-loglevel", "error",
                "-f", "lavfi", "-i", "testsrc2=size=160x120:rate=10",
                "-t", "2", "-c:v", "libx264", "-preset", "ultrafast",
                "-pix_fmt", "yuv420p", "-y",
            ])
            .arg(&path)
            .status()
            .ok()?;
        (status.success() && path.is_file()).then_some(path)
    }

    #[test]
    fn extract_frame_writes_a_real_jpeg() {
        // A regression test with teeth. The unit tests around this function
        // all passed while it was broken: the temporary file was named
        // `<hash>.jpg.part`, and ffmpeg refuses to guess a format from a
        // `.part` extension. Nothing short of running ffmpeg catches that.
        let dir = std::env::temp_dir().join(format!("wallflux-frame-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let Some(source) = sample_video(&dir) else {
            eprintln!("skipping: ffmpeg is not available");
            return;
        };

        let target = dir.join("abcdef0123456789.jpg");
        extract_frame(&source, &target).expect("extract_frame failed");

        assert!(target.is_file(), "no thumbnail was written");
        let bytes = std::fs::read(&target).unwrap();
        assert!(bytes.len() > 1000, "thumbnail is suspiciously small");
        // JPEG magic: FF D8 FF.
        assert_eq!(&bytes[..3], &[0xFF, 0xD8, 0xFF], "not a JPEG");
        // The temporary must not survive a successful run.
        assert!(!target.with_extension("part.jpg").exists());
    }

    #[test]
    fn extract_frame_reports_a_source_it_cannot_read() {
        let dir = std::env::temp_dir().join(format!("wallflux-badframe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("not-a-video.mp4");
        std::fs::write(&source, b"definitely not a video").unwrap();

        if Command::new("ffmpeg").arg("-version").output().is_err() {
            eprintln!("skipping: ffmpeg is not available");
            return;
        }
        let err = extract_frame(&source, &dir.join("out.jpg")).unwrap_err();
        assert!(
            format!("{err:#}").contains("could not read a frame"),
            "{err:#}"
        );
    }
}
