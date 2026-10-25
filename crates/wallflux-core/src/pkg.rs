//! Reader for Wallpaper Engine's `.pkg` scene container.
//!
//! The format is a plain, uncompressed and unencrypted archive: a version
//! string, a table of `(path, offset, length)`, then the data. It is not
//! documented by Valve — this follows the layout the open-source tooling
//! community settled on years ago — so **every field is treated as hostile**
//! and bounds-checked against the real file size before anything is read.
//!
//! What this gets you, and what it does not:
//!
//! * Extracting the container: this module. Straightforward.
//! * Playing a scene: **not possible here, and not planned.** A scene is a
//!   graph of layers, shaders, particle systems and audio-reactive logic;
//!   showing it means reimplementing Wallpaper Engine's renderer, which is a
//!   project of its own and partial at best in every attempt that exists.
//!
//! The useful middle ground is [`find_video`]: a good number of "scene"
//! wallpapers are really a video with effects layered on top, and when the
//! package holds a video file wallflux can play that and give you most of
//! the wallpaper. When it does not, the scene is procedural and the honest
//! answer is to say so.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::library::VIDEO_EXTENSIONS;

/// Version strings seen in the wild. An unknown one is read anyway — the
/// table layout has been stable — but it is reported so a surprise is
/// visible rather than silent.
const KNOWN_VERSIONS: &[&str] = &["PKGV0001", "PKGV0002", "PKGV0003", "PKGV0004"];

/// Nothing legitimate comes close to these. They exist so a corrupt or
/// hostile header cannot make wallflux allocate gigabytes.
const MAX_VERSION_LEN: i32 = 64;
const MAX_ENTRIES: i32 = 100_000;
const MAX_PATH_LEN: i32 = 4096;
const MAX_ENTRY_BYTES: u64 = 512 * 1024 * 1024;

/// One file inside the container.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Path as stored, using forward slashes.
    pub path: String,
    /// Offset from the start of the data section.
    pub offset: u64,
    pub length: u64,
}

/// What a container holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Archive {
    pub version: String,
    /// True when `version` is not one this build has seen before.
    pub unknown_version: bool,
    pub entries: Vec<Entry>,
    /// Byte offset where entry data begins.
    pub data_start: u64,
}

impl Archive {
    pub fn entry(&self, path: &str) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.path == path)
    }
}

/// Reads the header of a `.pkg` without touching its data.
pub fn read_archive(path: &Path) -> Result<Archive> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("no se puede abrir {}", path.display()))?;
    let total = file
        .metadata()
        .with_context(|| format!("no se puede medir {}", path.display()))?
        .len();
    let mut reader = std::io::BufReader::new(file);
    parse_header(&mut reader, total)
        .with_context(|| format!("{} no parece un .pkg válido", path.display()))
}

fn parse_header<R: Read + Seek>(reader: &mut R, total: u64) -> Result<Archive> {
    let version_len = read_i32(reader)?;
    if !(1..=MAX_VERSION_LEN).contains(&version_len) {
        bail!("longitud de versión fuera de rango ({version_len})");
    }
    let version = read_string(reader, version_len as usize)?;
    let unknown_version = !KNOWN_VERSIONS.contains(&version.as_str());

    let count = read_i32(reader)?;
    if !(0..=MAX_ENTRIES).contains(&count) {
        bail!("número de entradas fuera de rango ({count})");
    }

    let mut entries = Vec::with_capacity(count.min(1024) as usize);
    for index in 0..count {
        let path_len = read_i32(reader)?;
        if !(1..=MAX_PATH_LEN).contains(&path_len) {
            bail!("longitud de ruta fuera de rango en la entrada {index}");
        }
        let path = read_string(reader, path_len as usize)?;
        let offset = read_i32(reader)?;
        let length = read_i32(reader)?;
        if offset < 0 || length < 0 {
            bail!("desplazamiento o longitud negativos en {path}");
        }
        if length as u64 > MAX_ENTRY_BYTES {
            bail!("la entrada {path} dice medir {length} bytes");
        }
        entries.push(Entry {
            path: path.replace('\\', "/"),
            offset: offset as u64,
            length: length as u64,
        });
    }

    let data_start = reader.stream_position()?;

    // The table is self-describing and could claim anything; the file size is
    // the only fact. Checking here means the extraction loop can trust it.
    for entry in &entries {
        let end = data_start
            .checked_add(entry.offset)
            .and_then(|start| start.checked_add(entry.length))
            .context("desbordamiento al calcular el final de una entrada")?;
        if end > total {
            bail!(
                "la entrada {} se sale del archivo ({end} > {total})",
                entry.path
            );
        }
    }

    Ok(Archive {
        version,
        unknown_version,
        entries,
        data_start,
    })
}

/// Extracts every entry under `destination`, returning the paths written.
///
/// Paths inside the container are untrusted input: an entry called
/// `../../.bashrc` would otherwise write outside the destination. Each one is
/// rejected unless it stays inside.
pub fn extract(pkg: &Path, destination: &Path) -> Result<Vec<PathBuf>> {
    let archive = read_archive(pkg)?;
    let mut file = std::fs::File::open(pkg)?;
    crate::paths::ensure_dir(destination)?;

    let mut written = Vec::with_capacity(archive.entries.len());
    for entry in &archive.entries {
        let Some(relative) = safe_relative_path(&entry.path) else {
            tracing::warn!(path = %entry.path, "ruta insegura en el .pkg, ignorada");
            continue;
        };
        let target = destination.join(&relative);
        if let Some(parent) = target.parent() {
            crate::paths::ensure_dir(parent)?;
        }

        file.seek(SeekFrom::Start(archive.data_start + entry.offset))?;
        let mut data = vec![0u8; entry.length as usize];
        file.read_exact(&mut data)
            .with_context(|| format!("no se puede leer {} del .pkg", entry.path))?;
        std::fs::write(&target, &data)
            .with_context(|| format!("no se puede escribir {}", target.display()))?;
        written.push(target);
    }
    Ok(written)
}

/// Rejects anything that would escape the destination: absolute paths, `..`,
/// Windows roots and prefixes. Returns the cleaned relative path.
fn safe_relative_path(raw: &str) -> Option<PathBuf> {
    if raw.is_empty() || raw.starts_with('/') || raw.contains(':') {
        return None;
    }
    let mut clean = PathBuf::new();
    for component in Path::new(raw).components() {
        match component {
            Component::Normal(part) => clean.push(part),
            // `./` is harmless noise; everything else is a way out.
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!clean.as_os_str().is_empty()).then_some(clean)
}

/// The largest video file in an extracted package, if any.
///
/// This is what makes a scene usable without a renderer: when the package
/// carries a video, that video is the wallpaper in all but the effects
/// layered over it. Largest wins because packages often also contain a tiny
/// preview clip.
pub fn find_video(root: &Path) -> Option<PathBuf> {
    let mut best: Option<(u64, PathBuf)> = None;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let is_video = path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| VIDEO_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
                .unwrap_or(false);
            if !is_video {
                continue;
            }
            let size = entry.metadata().map(|meta| meta.len()).unwrap_or(0);
            if best.as_ref().map(|(best, _)| size > *best).unwrap_or(true) {
                best = Some((size, path));
            }
        }
    }
    best.map(|(_, path)| path)
}

fn read_i32<R: Read>(reader: &mut R) -> Result<i32> {
    let mut buffer = [0u8; 4];
    reader
        .read_exact(&mut buffer)
        .context("archivo truncado al leer un entero")?;
    Ok(i32::from_le_bytes(buffer))
}

fn read_string<R: Read>(reader: &mut R, len: usize) -> Result<String> {
    let mut buffer = vec![0u8; len];
    reader
        .read_exact(&mut buffer)
        .context("archivo truncado al leer una cadena")?;
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Builds a container in memory, the same way the format describes one.
    /// Writing the encoder makes the decoder's assumptions explicit, and
    /// gives the tests something real to chew on without shipping a binary
    /// fixture into the repository.
    fn build(version: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut header = Vec::new();
        header.extend((version.len() as i32).to_le_bytes());
        header.extend(version.as_bytes());
        header.extend((files.len() as i32).to_le_bytes());

        let mut offset: i32 = 0;
        for (path, data) in files {
            header.extend((path.len() as i32).to_le_bytes());
            header.extend(path.as_bytes());
            header.extend(offset.to_le_bytes());
            header.extend((data.len() as i32).to_le_bytes());
            offset += data.len() as i32;
        }
        for (_, data) in files {
            header.extend(*data);
        }
        header
    }

    fn tempdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wallflux-pkg-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_container_round_trips() {
        let bytes = build(
            "PKGV0001",
            &[("scene.json", b"{}"), ("materials/a.tex", b"textura")],
        );
        let total = bytes.len() as u64;
        let archive = parse_header(&mut Cursor::new(&bytes), total).unwrap();
        assert_eq!(archive.version, "PKGV0001");
        assert!(!archive.unknown_version);
        assert_eq!(archive.entries.len(), 2);
        assert_eq!(archive.entry("scene.json").unwrap().length, 2);
        assert_eq!(archive.entry("materials/a.tex").unwrap().offset, 2);
    }

    #[test]
    fn an_unknown_version_is_read_but_flagged() {
        // The table layout has been stable across versions, so refusing
        // outright would be worse than reading and saying so.
        let bytes = build("PKGV9999", &[("a", b"x")]);
        let archive = parse_header(&mut Cursor::new(&bytes), bytes.len() as u64).unwrap();
        assert!(archive.unknown_version);
        assert_eq!(archive.entries.len(), 1);
    }

    #[test]
    fn extraction_writes_the_files_and_their_directories() {
        let dir = tempdir("extract");
        let pkg = dir.join("scene.pkg");
        std::fs::write(
            &pkg,
            build(
                "PKGV0001",
                &[("scene.json", b"{\"x\":1}"), ("materials/a.tex", b"tex")],
            ),
        )
        .unwrap();

        let out = dir.join("out");
        let written = extract(&pkg, &out).unwrap();
        assert_eq!(written.len(), 2);
        assert_eq!(std::fs::read(out.join("scene.json")).unwrap(), b"{\"x\":1}");
        assert_eq!(std::fs::read(out.join("materials/a.tex")).unwrap(), b"tex");
    }

    #[test]
    fn path_traversal_entries_are_refused() {
        // The decisive test. Entry paths come out of an untrusted archive; an
        // entry named ../../.bashrc must never be written.
        assert_eq!(
            safe_relative_path("materials/a.tex"),
            Some(PathBuf::from("materials/a.tex"))
        );
        assert_eq!(safe_relative_path("./a"), Some(PathBuf::from("a")));
        assert_eq!(safe_relative_path("../../.bashrc"), None);
        assert_eq!(safe_relative_path("/etc/passwd"), None);
        assert_eq!(safe_relative_path("C:\\Windows\\system32"), None);
        assert_eq!(safe_relative_path("a/../../b"), None);
        assert_eq!(safe_relative_path(""), None);
    }

    #[test]
    fn a_traversing_entry_is_skipped_during_extraction() {
        let dir = tempdir("traverse");
        let pkg = dir.join("evil.pkg");
        std::fs::write(
            &pkg,
            build(
                "PKGV0001",
                &[("../escaped.txt", b"nope"), ("ok.txt", b"si")],
            ),
        )
        .unwrap();
        let out = dir.join("out");
        let written = extract(&pkg, &out).unwrap();
        assert_eq!(
            written.len(),
            1,
            "solo debería escribirse la entrada segura"
        );
        assert!(out.join("ok.txt").is_file());
        assert!(!dir.join("escaped.txt").exists());
    }

    #[test]
    fn an_entry_claiming_to_run_past_the_file_is_refused() {
        let mut bytes = build("PKGV0001", &[("a", b"12345")]);
        bytes.truncate(bytes.len() - 3); // los datos ya no están completos
        let total = bytes.len() as u64;
        let err = parse_header(&mut Cursor::new(&bytes), total)
            .unwrap_err()
            .to_string();
        assert!(err.contains("se sale del archivo"), "{err}");
    }

    #[test]
    fn absurd_headers_are_refused_before_allocating() {
        // entry count
        let mut bytes = Vec::new();
        bytes.extend(8i32.to_le_bytes());
        bytes.extend(b"PKGV0001");
        bytes.extend(i32::MAX.to_le_bytes());
        assert!(parse_header(&mut Cursor::new(&bytes), bytes.len() as u64).is_err());

        // version length
        let mut bytes = Vec::new();
        bytes.extend((-1i32).to_le_bytes());
        assert!(parse_header(&mut Cursor::new(&bytes), 4).is_err());
    }

    #[test]
    fn the_largest_video_wins() {
        let dir = tempdir("video");
        std::fs::create_dir_all(dir.join("materials")).unwrap();
        std::fs::write(dir.join("preview.mp4"), vec![0u8; 100]).unwrap();
        std::fs::write(dir.join("materials/main.mp4"), vec![0u8; 5000]).unwrap();
        std::fs::write(dir.join("scene.json"), b"{}").unwrap();
        assert_eq!(find_video(&dir), Some(dir.join("materials/main.mp4")));
    }

    #[test]
    fn a_procedural_scene_has_no_video_to_find() {
        let dir = tempdir("procedural");
        std::fs::write(dir.join("scene.json"), b"{}").unwrap();
        std::fs::write(dir.join("a.tex"), b"x").unwrap();
        assert_eq!(find_video(&dir), None);
    }
}
