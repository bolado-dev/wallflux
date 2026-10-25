//! Browsing the Wallpaper Engine Steam Workshop, and importing from it.
//!
//! Two halves with very different rules, and the distinction is the whole
//! reason this module is shaped the way it is:
//!
//! * **The listing is public.** Titles, previews, tags, sizes and authors
//!   come from `ISteamRemoteStorage/GetPublishedFileDetails`, which needs no
//!   API key and no account. The tags even say whether an item is a video or
//!   a scene, so unusable items are filtered out before anything is
//!   downloaded.
//! * **The files are not.** Every response carries an empty `file_url`:
//!   Steam publishes no direct download for this content, because Wallpaper
//!   Engine is a paid app. The only way to the bytes is a Steam account that
//!   owns it, which is what [`download_with_steamcmd`] uses — Valve's own
//!   tool, the user's own licence. Nothing here works around that gate, and
//!   nothing here should.
//!
//! So: anyone can browse. Owners can import.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::library::{VIDEO_EXTENSIONS, slugify};
use crate::paths;

/// Wallpaper Engine on Steam.
pub const APP_ID: &str = "431960";

const BROWSE_URL: &str = "https://steamcommunity.com/workshop/browse/";
const DETAILS_URL: &str =
    "https://api.steampowered.com/ISteamRemoteStorage/GetPublishedFileDetails/v1/";
const USER_AGENT: &str = concat!("wallflux/", env!("CARGO_PKG_VERSION"));

/// What kind of wallpaper an item is, read from its Workshop tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkshopKind {
    Video,
    Scene,
    Web,
    Application,
    Unknown,
}

impl WorkshopKind {
    fn from_tags(tags: &[String]) -> Self {
        for tag in tags {
            match tag.to_ascii_lowercase().as_str() {
                "video" => return WorkshopKind::Video,
                "scene" => return WorkshopKind::Scene,
                "web" => return WorkshopKind::Web,
                "application" => return WorkshopKind::Application,
                _ => {}
            }
        }
        WorkshopKind::Unknown
    }

    /// A video item always converts.
    pub fn importable(self) -> bool {
        self == WorkshopKind::Video
    }

    /// A scene *might*: wallflux cannot render a scene graph, but a good
    /// number of scenes are a video with effects layered over it, and the
    /// video inside the `.pkg` plays fine on its own. Whether a given scene
    /// is one of those is only knowable after downloading it, which is why
    /// this is a separate, weaker promise than [`Self::importable`].
    pub fn maybe_importable(self) -> bool {
        self == WorkshopKind::Scene
    }

    pub fn reason(self) -> &'static str {
        match self {
            WorkshopKind::Video => "se puede importar",
            WorkshopKind::Scene => "escena: solo funciona si lleva un vídeo dentro del .pkg",
            WorkshopKind::Web => "los fondos web aún no están soportados",
            WorkshopKind::Application => "los fondos de tipo aplicación son ejecutables",
            WorkshopKind::Unknown => "tipo de fondo no reconocido",
        }
    }
}

/// One Workshop item, as the public API describes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkshopItem {
    pub id: String,
    pub title: String,
    pub kind: WorkshopKind,
    pub preview_url: Option<String>,
    pub tags: Vec<String>,
    /// Bytes, as published. Useful before committing to a download.
    pub file_size: u64,
    /// From the resolution tag, e.g. `3840 x 2160`.
    pub resolution: Option<String>,
    /// Wallpaper Engine's own content rating: `Everyone`, `Mature`, …
    pub rating: Option<String>,
    /// The item's page on Steam.
    pub url: String,
}

/// How to order a browse request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    #[default]
    Trend,
    MostRecent,
    MostSubscribed,
}

impl Sort {
    fn as_param(self) -> &'static str {
        match self {
            Sort::Trend => "trend",
            Sort::MostRecent => "mostrecent",
            Sort::MostSubscribed => "totaluniquesubscribers",
        }
    }
}

/// Browses the Workshop. `query` empty means "whatever is popular".
///
/// Item ids come from the public browse page and the metadata from the
/// public API. The page scrape is the fragile half — it looks for
/// `filedetails/?id=NNN`, which has been the shape of a Workshop link for
/// years, but it is still HTML and could change. The metadata call that
/// follows is a documented, stable endpoint.
pub fn browse(query: &str, sort: Sort, page: u32) -> Result<Vec<WorkshopItem>> {
    let url = format!(
        "{BROWSE_URL}?appid={APP_ID}&browsesort={}&p={}&searchtext={}",
        sort.as_param(),
        page.max(1),
        urlencode(query)
    );
    let html = get_text(&url).context("no se ha podido leer el taller de Steam")?;
    let ids = extract_ids(&html);
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    details(&ids)
}

/// Public metadata for a batch of item ids. No API key: this endpoint is
/// open, which is what makes browsing possible without owning the app.
pub fn details(ids: &[String]) -> Result<Vec<WorkshopItem>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut form = format!("itemcount={}", ids.len());
    for (index, id) in ids.iter().enumerate() {
        form.push_str(&format!("&publishedfileids[{index}]={id}"));
    }

    let mut response = ureq::post(DETAILS_URL)
        .header("User-Agent", USER_AGENT)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(20)))
        .build()
        .send(&form)
        .context("la consulta de metadatos a Steam ha fallado")?;
    let payload: DetailsResponse = response
        .body_mut()
        .read_json()
        .context("Steam ha devuelto algo que no se puede interpretar")?;

    Ok(payload
        .response
        .publishedfiledetails
        .into_iter()
        .filter_map(to_item)
        .collect())
}

#[derive(Debug, Deserialize)]
struct DetailsResponse {
    response: DetailsInner,
}

#[derive(Debug, Deserialize)]
struct DetailsInner {
    #[serde(default)]
    publishedfiledetails: Vec<RawDetails>,
}

#[derive(Debug, Deserialize)]
struct RawDetails {
    #[serde(default)]
    result: u32,
    #[serde(default)]
    publishedfileid: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    preview_url: Option<String>,
    #[serde(default)]
    file_size: Option<serde_json::Value>,
    #[serde(default)]
    consumer_app_id: Option<u64>,
    #[serde(default)]
    tags: Vec<RawTag>,
}

#[derive(Debug, Deserialize)]
struct RawTag {
    #[serde(default)]
    tag: String,
}

fn to_item(raw: RawDetails) -> Option<WorkshopItem> {
    // result == 1 is Steam's "ok"; anything else is a deleted or private item.
    if raw.result != 1 {
        return None;
    }
    // Guard against an id from another game sneaking in through a stale link.
    if let Some(app) = raw.consumer_app_id
        && app.to_string() != APP_ID
    {
        return None;
    }

    let tags: Vec<String> = raw.tags.into_iter().map(|tag| tag.tag).collect();
    Some(WorkshopItem {
        kind: WorkshopKind::from_tags(&tags),
        title: raw
            .title
            .unwrap_or_else(|| format!("Elemento {}", raw.publishedfileid)),
        preview_url: raw.preview_url.filter(|url| !url.is_empty()),
        resolution: tags.iter().find(|tag| is_resolution(tag)).cloned(),
        rating: tags
            .iter()
            .find(|tag| matches!(tag.as_str(), "Everyone" | "Questionable" | "Mature"))
            .cloned(),
        file_size: raw
            .file_size
            .as_ref()
            .and_then(parse_size)
            .unwrap_or_default(),
        url: format!(
            "https://steamcommunity.com/sharedfiles/filedetails/?id={}",
            raw.publishedfileid
        ),
        id: raw.publishedfileid,
        tags,
    })
}

/// Steam returns `file_size` as a number in some responses and a string in
/// others; both have to work.
fn parse_size(value: &serde_json::Value) -> Option<u64> {
    match value {
        serde_json::Value::Number(number) => number.as_u64(),
        serde_json::Value::String(text) => text.parse().ok(),
        _ => None,
    }
}

/// `3840 x 2160` and friends.
fn is_resolution(tag: &str) -> bool {
    let mut parts = tag.split('x');
    let (Some(width), Some(height), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    width.trim().parse::<u32>().is_ok() && height.trim().parse::<u32>().is_ok()
}

/// Pulls Workshop item ids out of a browse page, in the order they appear.
pub fn extract_ids(html: &str) -> Vec<String> {
    const NEEDLE: &str = "filedetails/?id=";
    let mut ids = Vec::new();
    let mut rest = html;
    while let Some(position) = rest.find(NEEDLE) {
        rest = &rest[position + NEEDLE.len()..];
        let id: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if !id.is_empty() && !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

// --- Downloading, for people who own the app ---------------------------------

/// Where steamcmd lives, if it does.
pub fn steamcmd_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("WALLFLUX_STEAMCMD") {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    which("steamcmd")
}

fn which(binary: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(binary))
        .find(|candidate| candidate.is_file())
}

/// One way to install steamcmd.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallOption {
    /// Distro or method this is for.
    pub label: String,
    pub command: String,
    /// Caveat worth knowing before running it, if any.
    pub note: Option<String>,
}

/// How to install steamcmd, with the option for *this* machine first.
///
/// Reading `/etc/os-release` rather than listing every distro and letting
/// the user find theirs: the first command shown should be the one they can
/// paste.
pub fn install_options() -> Vec<InstallOption> {
    let os_release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let field = |key: &str| -> String {
        os_release
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .unwrap_or("")
            .trim_matches(['"', '\''])
            .to_ascii_lowercase()
    };
    let id = field("ID=");
    let id_like = field("ID_LIKE=");
    let is = |name: &str| id == name || id_like.split_whitespace().any(|part| part == name);

    let debian = InstallOption {
        label: "Debian, Ubuntu, Kali".to_string(),
        command: "sudo apt install steamcmd".to_string(),
        // The part people trip over: the package is in non-free, which is
        // not enabled by default, so apt reports "no installation candidate"
        // and it looks like the package does not exist.
        note: Some(
            "Está en el componente non-free. Si apt dice que no hay candidato, \
             añádelo a /etc/apt/sources.list y vuelve a intentarlo."
                .to_string(),
        ),
    };
    let arch = InstallOption {
        label: "Arch".to_string(),
        command: "yay -S steamcmd".to_string(),
        note: Some("Está en AUR, así que hace falta un ayudante como yay o paru.".to_string()),
    };
    let fedora = InstallOption {
        label: "Fedora".to_string(),
        command: "sudo dnf install steamcmd".to_string(),
        note: Some("Necesita el repositorio RPM Fusion no-free.".to_string()),
    };
    let flatpak = InstallOption {
        label: "Flatpak (cualquier distro)".to_string(),
        command: "flatpak install flathub com.valvesoftware.SteamCmd".to_string(),
        note: None,
    };
    let manual = InstallOption {
        label: "Manual".to_string(),
        command: "https://developer.valvesoftware.com/wiki/SteamCMD".to_string(),
        note: Some(
            "Descarga el tarball de Valve y apunta WALLFLUX_STEAMCMD al binario.".to_string(),
        ),
    };

    let mut options = Vec::new();
    if is("debian") {
        options.push(debian.clone());
    } else if is("arch") {
        options.push(arch.clone());
    } else if is("fedora") || is("rhel") {
        options.push(fedora.clone());
    }
    for option in [debian, arch, fedora, flatpak, manual] {
        if !options
            .iter()
            .any(|existing| existing.label == option.label)
        {
            options.push(option);
        }
    }
    options
}

/// Whether steamcmd already has a cached session for `user`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginCheck {
    pub logged_in: bool,
    /// What to tell the user, whichever way it went.
    pub message: String,
    /// The lines of steamcmd output that led to that verdict.
    ///
    /// Shown as small print rather than hidden: the first version of this
    /// check was wrong for a reason that was invisible without it, and
    /// guessing at someone else's steamcmd output is not debugging.
    pub detail: Option<String>,
}

/// Tests whether steamcmd can log in as `user` without asking anything.
///
/// Runs the real login with stdin closed and a deadline: if the session is
/// cached it returns immediately, and if it is not, steamcmd fails trying to
/// read a password instead of hanging forever waiting for one. That is the
/// only way to answer "am I logged in?" — there is no status command, and
/// poking at steamcmd's config files guesses at an undocumented layout.
pub fn check_login(user: &str) -> Result<LoginCheck> {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let user = user.trim();
    if user.is_empty() {
        bail!("escribe primero tu cuenta de Steam");
    }
    // El login anónimo conecta y parece correcto, pero no da acceso a
    // contenido de aplicaciones de pago. Dejarlo pasar sería prometer algo
    // que falla diez segundos más tarde, al descargar.
    if user.eq_ignore_ascii_case("anonymous") {
        bail!(
            "el login anónimo no sirve: el contenido del taller de Wallpaper Engine \
             solo está disponible para cuentas que poseen la aplicación"
        );
    }
    let steamcmd = steamcmd_path().with_context(|| steamcmd_help().to_string())?;

    let mut child = Command::new(&steamcmd)
        .arg("+login")
        .arg(user)
        .arg("+quit")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("no se ha podido ejecutar {}", steamcmd.display()))?;

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(status) = child.try_wait()? {
            let output = child.wait_with_output()?;
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            // El código de salida no se mira: steamcmd devuelve 0 también
            // cuando el login falla. Lo único que informa es la salida.
            let _ = status;
            let logged_in = looks_logged_in(&combined);
            return Ok(LoginCheck {
                message: if logged_in {
                    format!("Sesión activa como {user}. Ya puedes descargar.")
                } else {
                    explain_steamcmd_failure(&combined)
                },
                detail: significant_lines(&combined),
                logged_in,
            });
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            return Ok(LoginCheck {
                logged_in: false,
                message: "steamcmd se ha quedado esperando. Suele significar que pide \
                          contraseña: ejecútalo en una terminal para introducirla."
                    .to_string(),
                detail: None,
            });
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Markers that mean the login did **not** go through.
///
/// Deliberately specific phrases and not the bare word "failed". steamcmd
/// opens every run with
/// `WARNING: setlocale('en_US.UTF-8') failed, using locale: 'C'`,
/// so a substring match on "failed" reports a failure on every machine
/// whose locale it cannot set — which is most of them. That was a real bug
/// here, and it made the check answer "not logged in" to everybody.
const FAILURE_MARKERS: &[&str] = &[
    "cached credentials not found",
    "invalid password",
    "login failure",
    "failed login",
    "account logon denied",
    "rate limit exceeded",
    "two-factor code mismatch",
    "error (",
];

/// Markers that only appear once authentication has succeeded.
const SUCCESS_MARKERS: &[&str] = &[
    "logging in using cached credentials",
    "waiting for user info",
    "logged in ok",
];

/// Did steamcmd actually authenticate?
///
/// Read from the output, because the exit code is worthless: steamcmd
/// returns 0 after a failed login too. Checked against captured output from
/// a real steamcmd, both the success and the failure path.
fn looks_logged_in(output: &str) -> bool {
    let lower = output.to_lowercase();
    if FAILURE_MARKERS.iter().any(|marker| lower.contains(marker)) {
        return false;
    }
    SUCCESS_MARKERS.iter().any(|marker| lower.contains(marker))
}

/// The handful of output lines worth showing a human, with the noise
/// (progress bars, update checks, the locale warning) left out.
fn significant_lines(output: &str) -> Option<String> {
    let kept: Vec<String> = output
        .lines()
        .map(strip_ansi)
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .filter(|line| {
            let lower = line.to_lowercase();
            !lower.contains("setlocale")
                && !lower.contains("checking for available updates")
                && !lower.contains("verifying installation")
                && !lower.contains("downloading update")
                && !lower.contains("redirecting stderr")
                && !lower.contains("logging directory")
                && !lower.starts_with("[----]")
        })
        .collect();
    (!kept.is_empty()).then(|| kept.join("\n"))
}

/// Removes the CSI escape sequences steamcmd sprinkles through its output.
/// They render as literal noise in a GUI.
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            out.push(ch);
            continue;
        }
        // CSI: ESC [ parameters, ending in a letter.
        for next in chars.by_ref() {
            if next.is_ascii_alphabetic() {
                break;
            }
        }
    }
    out
}

/// Advice for a machine with no steamcmd, written to be actionable rather
/// than a shrug. Debian and Kali keep it in non-free, which is not enabled
/// by default, so "apt install steamcmd" alone is often not enough.
pub fn steamcmd_help() -> &'static str {
    "steamcmd no está instalado. Es la herramienta oficial de Valve y hace falta \
     para descargar del taller con tu cuenta.\n\
     \n\
     Debian/Kali/Ubuntu:  sudo apt install steamcmd        (necesita el componente non-free)\n\
     Arch:                yay -S steamcmd\n\
     Flatpak:             flatpak install flathub com.valvesoftware.SteamCmd\n\
     Manual:              https://developer.valvesoftware.com/wiki/SteamCMD\n\
     \n\
     Si lo tienes en una ruta poco habitual, apúntala con WALLFLUX_STEAMCMD."
}

/// Whether the logged-in account owns Wallpaper Engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ownership {
    Owned,
    NotOwned,
    /// steamcmd truncated its licence listing, so absence proves nothing.
    Unknown,
}

/// Asks steamcmd which licences the account holds and looks for this app.
///
/// `licenses_print` is read-only and fast, unlike the other way of finding
/// out — asking to install the app and reading the refusal — which starts a
/// download when the answer is yes.
///
/// The catch is that steamcmd abbreviates long app lists with
/// `... (196 in total)`, so a missing id only proves anything when nothing
/// was abbreviated. Hence three states rather than a boolean: claiming
/// "you do not own this" from a truncated list would be a guess dressed up
/// as a fact.
pub fn check_ownership(user: &str) -> Result<Ownership> {
    let steamcmd = steamcmd_path().with_context(|| steamcmd_help().to_string())?;
    let output = Command::new(&steamcmd)
        .arg("+login")
        .arg(user.trim())
        .arg("+licenses_print")
        .arg("+quit")
        .stdin(std::process::Stdio::null())
        .output()
        .with_context(|| format!("no se ha podido ejecutar {}", steamcmd.display()))?;

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    match parse_ownership(&combined, APP_ID) {
        // A positive from the licence list is certain; take it and stop.
        Ownership::Owned => Ok(Ownership::Owned),
        // Not conclusive, because steamcmd abbreviates long lists. Ask the
        // question that cannot be abbreviated.
        _ => probe_ownership(&steamcmd, user.trim()),
    }
}

/// Asks steamcmd to install the app and reads only the first verdict.
///
/// `ERROR! Failed to install app '431960' (No subscription)` is unambiguous
/// and arrives immediately. The reason this is not the first thing tried is
/// the other branch: when the account *does* own it, steamcmd starts
/// downloading the whole application. So the output is read line by line and
/// the process is killed the instant either answer appears — nothing is left
/// running and no real download begins.
fn probe_ownership(steamcmd: &Path, user: &str) -> Result<Ownership> {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let scratch = crate::paths::cache_dir()?.join("ownership-probe");
    crate::paths::ensure_dir(&scratch)?;

    let mut child = Command::new(steamcmd)
        .arg("+force_install_dir")
        .arg(&scratch)
        .arg("+login")
        .arg(user)
        .arg("+app_update")
        .arg(APP_ID)
        .arg("+quit")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("no se ha podido ejecutar {}", steamcmd.display()))?;

    let verdict = (|| {
        let stdout = child.stdout.take()?;
        let deadline = Instant::now() + Duration::from_secs(90);
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let lower = strip_ansi(&line).to_lowercase();
            if lower.contains("no subscription") {
                return Some(Ownership::NotOwned);
            }
            // Señales de que pasó el control de licencia y empezó a trabajar.
            //
            // Nada de "verifying install": steamcmd imprime «Verifying
            // installation...» nada más arrancar, antes incluso del login, y
            // esa línea llega antes que el «No subscription». Darla por
            // buena hacía que la sonda afirmase lo contrario de la verdad.
            if lower.contains("update state")
                || lower.contains("downloading, progress")
                || lower.starts_with("success! app")
                || lower.contains("fully installed")
            {
                return Some(Ownership::Owned);
            }
            if Instant::now() > deadline {
                return None;
            }
        }
        None
    })();

    // Whatever happened, do not leave steamcmd pulling an application down.
    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_dir_all(&scratch);

    Ok(verdict.unwrap_or(Ownership::Unknown))
}

fn parse_ownership(output: &str, app_id: &str) -> Ownership {
    let mut truncated = false;
    for line in output.lines() {
        let line = strip_ansi(line);
        let Some((_, apps)) = line.split_once("Apps") else {
            continue;
        };
        let Some((_, apps)) = apps.split_once(':') else {
            continue;
        };
        if apps.contains("...") || apps.contains("in total") {
            truncated = true;
        }
        if apps
            .split(',')
            .map(|part| part.trim().trim_end_matches(|c: char| !c.is_ascii_digit()))
            .any(|id| id == app_id)
        {
            return Ownership::Owned;
        }
    }
    if truncated {
        Ownership::Unknown
    } else {
        Ownership::NotOwned
    }
}

/// Result of a download attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Downloaded {
    /// Directory steamcmd left the item in.
    pub path: PathBuf,
}

/// Downloads one Workshop item using the user's own Steam account.
///
/// This shells out to steamcmd rather than speaking the Steam protocol: it
/// is Valve's supported path, it handles Steam Guard, and it means wallflux
/// never sees the password.
///
/// `steam_user` must own Wallpaper Engine. Credentials are steamcmd's
/// business, not ours — run `steamcmd +login <user> +quit` once in a
/// terminal to cache them, and this call then runs unattended.
pub fn download_with_steamcmd(
    item_id: &str,
    steam_user: &str,
    install_dir: &Path,
) -> Result<Downloaded> {
    if !item_id.chars().all(|c| c.is_ascii_digit()) {
        bail!("el id del taller debe ser numérico");
    }
    if steam_user.trim().is_empty() {
        bail!("hace falta un usuario de Steam que tenga Wallpaper Engine");
    }
    let steamcmd = steamcmd_path().with_context(|| steamcmd_help().to_string())?;
    paths::ensure_dir(install_dir)?;

    let output = Command::new(&steamcmd)
        .arg("+force_install_dir")
        .arg(install_dir)
        .arg("+login")
        .arg(steam_user.trim())
        .arg("+workshop_download_item")
        .arg(APP_ID)
        .arg(item_id)
        .arg("+quit")
        .output()
        .with_context(|| format!("no se ha podido ejecutar {}", steamcmd.display()))?;

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let path = install_dir
        .join("steamapps/workshop/content")
        .join(APP_ID)
        .join(item_id);
    if !path.is_dir() {
        // La explicación interpretada, y debajo lo que dijo steamcmd: una de
        // las dos sobra hasta que no sobra.
        let mut message = explain_steamcmd_failure(&combined);
        // Una denegación genérica merece una respuesta firme en vez de un
        // "casi siempre significa": si el fallo apunta a la titularidad,
        // se comprueba de verdad antes de echarle la culpa a la cuenta.
        if combined.to_lowercase().contains("failed (failure)")
            && let Ok(ownership) = check_ownership(steam_user)
        {
            message = match ownership {
                Ownership::NotOwned => format!(
                    "Confirmado: la cuenta {} no tiene Wallpaper Engine en sus \
                     licencias, y el contenido de su taller solo está disponible \
                     para quien posee la aplicación.",
                    steam_user.trim()
                ),
                Ownership::Owned => format!(
                    "La cuenta {} sí tiene Wallpaper Engine, así que el problema no \
                     es la licencia. Puede ser un elemento retirado o restringido; \
                     prueba con otro.",
                    steam_user.trim()
                ),
                // Sigue sin poder afirmarse nada: se deja la redacción
                // prudente en vez de inventar una certeza.
                Ownership::Unknown => message,
            };
        }
        match significant_lines(&combined) {
            Some(detail) => bail!("{message}\n\n--- lo que dijo steamcmd ---\n{detail}"),
            None => bail!("{message}"),
        }
    }
    Ok(Downloaded { path })
}

/// Turns steamcmd's output into something a user can act on. Its own
/// messages are terse and it exits 0 on several kinds of failure, so the
/// output is all there is to go on.
pub fn explain_steamcmd_failure(output: &str) -> String {
    let lower = output.to_lowercase();
    // El caso más común con diferencia, y el que tiene la instrucción más
    // concreta: nunca se ha hecho el login interactivo en esta máquina.
    if lower.contains("cached credentials not found") {
        return "steamcmd no tiene sesión guardada para esa cuenta. Ejecuta \
                `steamcmd +login <usuario> +quit` en una terminal: te pedirá la \
                contraseña y el código de Steam Guard una sola vez."
            .to_string();
    }
    if lower.contains("invalid password") || lower.contains("login failure") {
        return "Steam ha rechazado el inicio de sesión. Ejecuta \
                `steamcmd +login <usuario> +quit` en una terminal para \
                introducir la contraseña y el Steam Guard una vez."
            .to_string();
    }
    if lower.contains("two-factor") || lower.contains("steam guard") {
        return "Steam Guard pide un código. Ejecuta `steamcmd +login <usuario> +quit` \
                en una terminal para autenticarte una vez; después wallflux podrá \
                descargar sin intervención."
            .to_string();
    }
    // El caso real medido: con sesión válida y una app de pago que la cuenta
    // no posee, steamcmd dice exactamente «ERROR! Download item N failed
    // (Failure).» — sin más pistas. Verificado contra un item del taller de
    // una app gratuita, que con la misma cuenta y la misma sesión descarga
    // sin problema: lo que falta es la titularidad, no la autenticación.
    if lower.contains("failed (failure)")
        || lower.contains("access denied")
        || (lower.contains("failure") && lower.contains("0x"))
    {
        return "Steam ha denegado la descarga pese a tener la sesión iniciada. \
                Casi siempre significa que esa cuenta no posee Wallpaper Engine: \
                el contenido de su taller solo está disponible para quien tiene la \
                aplicación. Compruébalo en tu biblioteca de Steam."
            .to_string();
    }
    if lower.contains("timeout") {
        return "steamcmd ha agotado el tiempo de espera. Puede ser la red, o Steam \
                teniendo un mal día."
            .to_string();
    }
    "steamcmd ha terminado sin dejar la carpeta del elemento. Ejecútalo a mano para \
     ver el motivo: steamcmd +login <usuario> +workshop_download_item 431960 <id> +quit"
        .to_string()
}

/// Turns a directory steamcmd downloaded into a wallflux package.
///
/// Reuses the Wallpaper Engine layout: a `project.json` manifest next to the
/// assets. Only video items convert; everything else is refused by name.
pub fn import_downloaded(path: &Path, library_root: &Path) -> Result<String> {
    let manifest_path = path.join("project.json");
    let text = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("no se puede leer {}", manifest_path.display()))?;
    let project: ProjectJson = serde_json::from_str(&text)
        .with_context(|| format!("{} no es JSON válido", manifest_path.display()))?;

    let kind = project
        .kind
        .as_deref()
        .map(|raw| WorkshopKind::from_tags(&[raw.to_string()]))
        .unwrap_or(WorkshopKind::Unknown);
    if !kind.importable() && !kind.maybe_importable() {
        bail!("{}", kind.reason());
    }

    // A video item names its file directly. A scene names a `.pkg`, which
    // has to be opened to find out whether there is a video in there at all.
    // `_staging` holds the extraction directory alive until the copy below.
    let _staging;
    let source = match kind {
        WorkshopKind::Video => project
            .file
            .as_deref()
            .map(|file| path.join(file))
            .filter(|candidate| candidate.is_file() && has_video_extension(candidate))
            .context("el manifiesto no apunta a ningún archivo de vídeo")?,
        _ => {
            let (video, staging) = video_from_scene(path)?;
            _staging = staging;
            video
        }
    };

    let title = project
        .title
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| {
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("wallpaper")
                .to_string()
        });

    paths::ensure_dir(library_root)?;
    let id = unique_id(library_root, &slugify(&title));
    let package = library_root.join(&id);
    std::fs::create_dir(&package)
        .with_context(|| format!("no se puede crear {}", package.display()))?;

    let extension = source
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("mp4")
        .to_ascii_lowercase();
    std::fs::copy(&source, package.join(format!("video.{extension}")))
        .with_context(|| format!("no se puede copiar {}", source.display()))?;

    if let Some(preview) = project.preview.as_deref() {
        let preview_path = path.join(preview);
        if let Some(ext) = preview_path.extension().and_then(|ext| ext.to_str()) {
            let _ = std::fs::copy(&preview_path, package.join(format!("preview.{ext}")));
        }
    }

    let mut manifest = format!("[wallpaper]\ntitle = {title:?}\nkind = \"video\"\n");
    if !project.tags.is_empty() {
        let tags: Vec<String> = project.tags.iter().map(|tag| format!("{tag:?}")).collect();
        manifest.push_str(&format!("tags = [{}]\n", tags.join(", ")));
    }
    manifest.push_str(&format!(
        "\n# Descargado del taller de Steam con tu propia cuenta\n\
         # https://steamcommunity.com/sharedfiles/filedetails/?id={}\n",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("?")
    ));
    manifest.push_str(&format!("\n[video]\nfile = \"video.{extension}\"\n"));
    std::fs::write(package.join("wallflux.toml"), manifest).with_context(|| {
        format!(
            "no se puede escribir el manifiesto en {}",
            package.display()
        )
    })?;

    Ok(id)
}

/// Opens a scene's `.pkg` and returns the video inside, if there is one.
///
/// Returns the extraction directory alongside it so the caller can copy the
/// file before it is cleaned up.
fn video_from_scene(item: &Path) -> Result<(PathBuf, ScratchDir)> {
    let pkg = find_pkg(item).context(
        "la escena no trae ningún .pkg que abrir; puede que use un formato que wallflux no conoce",
    )?;
    let staging = ScratchDir::new(item.join(".wallflux-extract"))?;
    crate::pkg::extract(&pkg, &staging.path)
        .with_context(|| format!("no se ha podido abrir {}", pkg.display()))?;

    let video = crate::pkg::find_video(&staging.path).context(
        "esta escena no lleva vídeo dentro: es procedural (shaders, partículas) y \
         reproducirla exigiría el motor de Wallpaper Engine, que wallflux no implementa",
    )?;
    Ok((video, staging))
}

fn find_pkg(dir: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    entries.flatten().map(|entry| entry.path()).find(|path| {
        path.is_file()
            && path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("pkg"))
                .unwrap_or(false)
    })
}

/// A directory removed when it goes out of scope. An extracted scene can be
/// hundreds of megabytes of textures we do not keep.
struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new(path: PathBuf) -> Result<Self> {
        let _ = std::fs::remove_dir_all(&path);
        crate::paths::ensure_dir(&path)?;
        Ok(Self { path })
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

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
}

fn has_video_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| VIDEO_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
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

fn urlencode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

fn get_text(url: &str) -> Result<String> {
    let mut response = ureq::get(url)
        .header("User-Agent", USER_AGENT)
        .config()
        .timeout_global(Some(std::time::Duration::from_secs(25)))
        .build()
        .call()?;
    Ok(response.body_mut().read_to_string()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_come_from_the_workshop_tags() {
        let tags = |values: &[&str]| -> Vec<String> {
            values.iter().map(|value| value.to_string()).collect()
        };
        assert_eq!(
            WorkshopKind::from_tags(&tags(&["Video", "Anime", "1920 x 1080"])),
            WorkshopKind::Video
        );
        assert_eq!(
            WorkshopKind::from_tags(&tags(&["Scene", "Audio responsive"])),
            WorkshopKind::Scene
        );
        // Tags arrive capitalised from Steam but that is not guaranteed.
        assert_eq!(WorkshopKind::from_tags(&tags(&["web"])), WorkshopKind::Web);
        assert_eq!(
            WorkshopKind::from_tags(&tags(&["Wallpaper"])),
            WorkshopKind::Unknown
        );
    }

    #[test]
    fn only_video_items_are_importable() {
        assert!(WorkshopKind::Video.importable());
        for kind in [
            WorkshopKind::Scene,
            WorkshopKind::Web,
            WorkshopKind::Application,
            WorkshopKind::Unknown,
        ] {
            assert!(!kind.importable());
            assert!(!kind.reason().is_empty());
        }
    }

    #[test]
    fn ids_are_pulled_from_a_browse_page_without_duplicates() {
        let html = r#"
            <a href="https://steamcommunity.com/sharedfiles/filedetails/?id=3825386273">x</a>
            <a href="/sharedfiles/filedetails/?id=3825386273&searchtext=">dup</a>
            <a href="/sharedfiles/filedetails/?id=3824326992">y</a>
            <a href="/app/431960">no es un item</a>
        "#;
        assert_eq!(extract_ids(html), vec!["3825386273", "3824326992"]);
    }

    #[test]
    fn an_empty_page_yields_nothing_rather_than_garbage() {
        assert!(extract_ids("<html><body>nada</body></html>").is_empty());
    }

    #[test]
    fn resolution_tags_are_recognised() {
        assert!(is_resolution("3840 x 2160"));
        assert!(is_resolution("1920x1080"));
        assert!(!is_resolution("Anime"));
        assert!(!is_resolution("1920 x 1080 x 30"));
    }

    #[test]
    fn file_size_parses_as_number_or_string() {
        use serde_json::json;
        assert_eq!(parse_size(&json!(37104544)), Some(37104544));
        assert_eq!(parse_size(&json!("37104544")), Some(37104544));
        assert_eq!(parse_size(&json!("no")), None);
    }

    #[test]
    fn deleted_or_foreign_items_are_dropped() {
        // result != 1 means deleted or private.
        let deleted = RawDetails {
            result: 9,
            publishedfileid: "1".into(),
            title: Some("Borrado".into()),
            preview_url: None,
            file_size: None,
            consumer_app_id: Some(431960),
            tags: vec![],
        };
        assert!(to_item(deleted).is_none());

        // An id for a different game, reached through a stale link.
        let foreign = RawDetails {
            result: 1,
            publishedfileid: "2".into(),
            title: Some("De otro juego".into()),
            preview_url: None,
            file_size: None,
            consumer_app_id: Some(123),
            tags: vec![],
        };
        assert!(to_item(foreign).is_none());
    }

    #[test]
    fn an_item_keeps_its_resolution_rating_and_page() {
        let raw = RawDetails {
            result: 1,
            publishedfileid: "3825386273".into(),
            title: Some("Rainy Day".into()),
            preview_url: Some("https://images.steamusercontent.com/x".into()),
            file_size: Some(serde_json::json!("37104544")),
            consumer_app_id: Some(431960),
            tags: vec![
                RawTag {
                    tag: "Video".into(),
                },
                RawTag {
                    tag: "3840 x 2160".into(),
                },
                RawTag {
                    tag: "Everyone".into(),
                },
            ],
        };
        let item = to_item(raw).unwrap();
        assert_eq!(item.kind, WorkshopKind::Video);
        assert_eq!(item.resolution.as_deref(), Some("3840 x 2160"));
        assert_eq!(item.rating.as_deref(), Some("Everyone"));
        assert_eq!(item.file_size, 37104544);
        assert!(item.url.ends_with("id=3825386273"));
    }

    #[test]
    fn install_options_put_this_machine_first_and_list_the_rest() {
        let options = install_options();
        assert!(options.len() >= 5, "deberían estar todas las alternativas");
        // Sin duplicados aunque la distro detectada ya estuviese en la lista.
        let mut labels: Vec<_> = options.iter().map(|option| &option.label).collect();
        let before = labels.len();
        labels.sort();
        labels.dedup();
        assert_eq!(labels.len(), before);
        // Flatpak y manual siempre valen, da igual la distro.
        assert!(
            options
                .iter()
                .any(|option| option.command.contains("flatpak"))
        );
        assert!(
            options
                .iter()
                .any(|option| option.command.contains("developer.valvesoftware"))
        );
    }

    /// Salida real de un steamcmd que SÍ entra (login anónimo), tal cual.
    const REAL_SUCCESS: &str = concat!(
        "WARNING: setlocale('en_US.UTF-8') failed, using locale: 'C'.",
        " International characters may not work.\n",
        "Steam Console Client (c) Valve Corporation\n",
        "Loading Steam API...\u{1b}[0mOK\n",
        "Connecting anonymously to Steam Public...\u{1b}[0mOK\n",
        "Waiting for client config...\u{1b}[0mOK\n",
        "Waiting for user info...\u{1b}[0mOK\n",
    );

    /// Salida real cuando no hay sesión guardada y stdin está cerrado.
    const REAL_FAILURE: &str = concat!(
        "WARNING: setlocale('en_US.UTF-8') failed, using locale: 'C'.\n",
        "[----] Verifying installation...\n",
        "Loading Steam API...\u{1b}[0mOK\n",
        "\u{1b}[1mCached credentials not found.\n",
        "password: \u{1b}[0m\u{1b}[1m\n",
        "Logging in user 'x' [U:1:0] to Steam Public...\u{1b}[0mERROR (Invalid Password)\n",
    );

    #[test]
    fn the_locale_warning_does_not_count_as_a_failure() {
        // El bug que hubo: steamcmd abre cada ejecución con
        // "setlocale(...) failed", así que un contains("failed") daba
        // "no has iniciado sesión" a todo el mundo, hiciera lo que hiciera.
        assert!(
            REAL_SUCCESS.contains("failed"),
            "el aviso tiene que estar en el fixture"
        );
        assert!(looks_logged_in(REAL_SUCCESS));
    }

    #[test]
    fn the_real_failure_output_is_recognised() {
        assert!(!looks_logged_in(REAL_FAILURE));
        let advice = explain_steamcmd_failure(REAL_FAILURE);
        assert!(advice.contains("no tiene sesión guardada"), "{advice}");
    }

    #[test]
    fn the_detail_drops_the_noise_and_the_escape_codes() {
        let detail = significant_lines(REAL_FAILURE).unwrap();
        assert!(!detail.contains("setlocale"), "sobra el aviso de locale");
        assert!(!detail.contains("Verifying installation"));
        assert!(!detail.contains('\u{1b}'), "no deben quedar códigos ANSI");
        assert!(detail.contains("Cached credentials not found"));
        assert!(detail.contains("ERROR (Invalid Password)"));
    }

    #[test]
    fn stripping_ansi_leaves_the_text_alone() {
        assert_eq!(strip_ansi("Loading\u{1b}[0mOK"), "LoadingOK");
        assert_eq!(strip_ansi("sin códigos"), "sin códigos");
        assert_eq!(strip_ansi("\u{1b}[1mnegrita\u{1b}[0m"), "negrita");
    }

    #[test]
    fn login_detection_matches_real_steamcmd_output() {
        // Capturado de un steamcmd de verdad, códigos ANSI incluidos. La
        // documentación oficiosa habla de "Logged in OK"; la salida real no
        // lo dice, y por eso la detección mira "Waiting for user info".
        let real_success = "Loading Steam API...\u{1b}[0mOK\n             Connecting anonymously to Steam Public...\u{1b}[0mOK\n             Waiting for client config...\u{1b}[0mOK\n             Waiting for user info...\u{1b}[0mOK\n";
        assert!(looks_logged_in(real_success));

        assert!(!looks_logged_in(
            "Logging in user 'x' to Steam Public...\nFAILED login with result code Invalid Password"
        ));
        // Un FAILED posterior invalida la sesión aunque antes se avanzara.
        assert!(!looks_logged_in(
            "Waiting for user info...OK\nFAILED (Rate Limit Exceeded)"
        ));
        assert!(!looks_logged_in(""));
    }

    #[test]
    fn the_anonymous_account_is_refused_with_the_reason() {
        let err = check_login("anonymous").unwrap_err().to_string();
        assert!(err.contains("poseen la aplicación"), "{err}");
        assert!(check_login("ANONYMOUS").is_err(), "sin importar mayúsculas");
    }

    #[test]
    fn checking_a_login_without_a_user_says_so() {
        let err = check_login("   ").unwrap_err().to_string();
        assert!(err.contains("cuenta de Steam"), "{err}");
    }

    /// Formato real de `licenses_print`, con la abreviación incluida.
    const REAL_LICENSES: &str = concat!(
        "License packageID 0:\n",
        " - State   : Active (flags 0x200) - Purchased : Mon Oct 18 2021, Complimentary\n",
        " - Apps\t: 5, 7, 8, 90, 205, 211, 440, 570, ... (196 in total)\n",
        "License packageID 19007:\n",
        " - Apps\t: 227300, 388478, 421800, 1650650,  (13 in total)\n",
    );

    /// Las líneas que steamcmd escupe antes de hacer nada útil. Ninguna
    /// puede contar como señal de que la cuenta posee la aplicación.
    #[test]
    fn startup_noise_is_not_mistaken_for_ownership() {
        for noise in [
            "[----] Verifying installation...",
            "Steam Console Client (c) Valve Corporation",
            "Loading Steam API...OK",
            "UpdateUI: skip show logo",
        ] {
            let lower = strip_ansi(noise).to_lowercase();
            let looks_owned = lower.contains("update state")
                || lower.contains("downloading, progress")
                || lower.starts_with("success! app")
                || lower.contains("fully installed");
            assert!(!looks_owned, "«{noise}» no debería implicar titularidad");
        }
    }

    #[test]
    fn ownership_says_unknown_when_the_listing_was_abbreviated() {
        // El id podría estar en el trozo que steamcmd no imprimió, así que
        // afirmar "no la tienes" sería una suposición disfrazada de dato.
        assert_eq!(parse_ownership(REAL_LICENSES, "431960"), Ownership::Unknown);
    }

    #[test]
    fn ownership_is_certain_when_the_app_is_listed() {
        assert_eq!(parse_ownership(REAL_LICENSES, "440"), Ownership::Owned);
        assert_eq!(parse_ownership(REAL_LICENSES, "227300"), Ownership::Owned);
    }

    #[test]
    fn ownership_is_a_definite_no_when_nothing_was_abbreviated() {
        let complete = "License packageID 1:\n - Apps\t: 100, 200, 300,  (3 in total)\n";
        // Ojo: "(3 in total)" también cuenta como abreviado por prudencia,
        // así que un listado realmente completo no lleva esa coletilla.
        let plain = "License packageID 1:\n - Apps\t: 100, 200, 300\n";
        assert_eq!(parse_ownership(plain, "431960"), Ownership::NotOwned);
        assert_eq!(parse_ownership(plain, "200"), Ownership::Owned);
        assert_eq!(parse_ownership(complete, "431960"), Ownership::Unknown);
    }

    #[test]
    fn a_denied_download_blames_ownership_not_the_login() {
        // Salida real: sesión correcta, app de pago que la cuenta no tiene.
        let output = "Logging in user 'x' to Steam Public...OK\n\
                      Waiting for user info...OK\n\
                      Downloading item 3822803595 ...\n\
                      ERROR! Download item 3822803595 failed (Failure).";
        let advice = explain_steamcmd_failure(output);
        assert!(advice.contains("no posee Wallpaper Engine"), "{advice}");
        // Y no debe confundirse con un problema de autenticación.
        assert!(!advice.contains("contraseña"), "{advice}");
    }

    #[test]
    fn a_cached_session_counts_as_logged_in() {
        // Línea observada en una sesión ya guardada.
        assert!(looks_logged_in(
            "Logging in using cached credentials.\n\
             Logging in user 'x' [U:1:1] to Steam Public...OK"
        ));
    }

    #[test]
    fn steamcmd_failures_are_translated_into_advice() {
        assert!(
            explain_steamcmd_failure("FAILED login with result code Invalid Password")
                .contains("rechazado el inicio de sesión")
        );
        assert!(explain_steamcmd_failure("Steam Guard code required").contains("Steam Guard"));
        assert!(
            explain_steamcmd_failure("ERROR! Download item failed (Access Denied).")
                .contains("no posee Wallpaper Engine")
        );
        // Anything unrecognised still tells the user what to run by hand.
        assert!(explain_steamcmd_failure("vaya").contains("workshop_download_item"));
    }

    #[test]
    fn a_numeric_id_is_required_before_anything_is_executed() {
        let dir = std::env::temp_dir();
        let err = download_with_steamcmd("; rm -rf /", "alguien", &dir).unwrap_err();
        assert!(err.to_string().contains("numérico"), "{err}");
    }

    #[test]
    fn importing_a_video_produces_a_package_the_daemon_reads() {
        let dir = std::env::temp_dir().join(format!("wallflux-wsv-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let item = dir.join("456");
        std::fs::create_dir_all(&item).unwrap();
        std::fs::write(item.join("scene.mp4"), b"not actually a video").unwrap();
        std::fs::write(item.join("preview.jpg"), b"x").unwrap();
        std::fs::write(
            item.join("project.json"),
            r#"{"title":"Rainy Day","type":"video","file":"scene.mp4",
                "preview":"preview.jpg","tags":["Cartoon"]}"#,
        )
        .unwrap();

        let library = dir.join("lib");
        let id = import_downloaded(&item, &library).unwrap();
        assert_eq!(id, "rainy-day");
        assert!(library.join(&id).join("video.mp4").is_file());
        assert!(library.join(&id).join("preview.jpg").is_file());

        let found = crate::library::Library::load_from(&library).unwrap();
        assert!(found.errors.is_empty(), "{:?}", found.errors);
        assert_eq!(found.get("rainy-day").unwrap().title, "Rainy Day");
    }
}
