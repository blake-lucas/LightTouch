//! Legacy Store (legacystore.app) client, ported from the Android app's LegacyStore.kt: search the archive of
//! classic iOS apps for one guest, browse it like the website (categories, collections, charts), list the archived
//! versions that run on the guest, and download one copy after checking it against the guest again. The MD5 checks
//! archive integrity, not who published the app.
use crate::download::agent;
use crate::target::{Target, requires_newer, version};
use serde::Deserialize;
use serde_json::Value;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const BASE: &str = "https://legacystore.app";
const JSON_LIMIT: u64 = 8 << 20;
const DOWNLOAD_LIMIT: u64 = 2 << 30;
/// Archived copies whose executable is cut short, each with an intact copy of the same version.
const BROKEN: &[(i64, i64)] = &[(59853, 179589)]; // Fruit Ninja 1.7.4: the search row's copy

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    Cancelled,
    Failed(String),
    /// The catalog answered with this HTTP status.
    Status(u16, String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("Cancelled."),
            Self::Failed(message) | Self::Status(_, message) => f.write_str(message),
        }
    }
}

impl std::error::Error for StoreError {}

fn fail<T>(message: impl Into<String>) -> Result<T, StoreError> {
    Err(StoreError::Failed(message.into()))
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Compat {
    #[serde(default = "yes")]
    pub compatible: bool,
    #[serde(default)]
    pub reasons: Vec<String>,
}

fn yes() -> bool {
    true
}

/// One app in a list. Search rows carry the archived copy the catalog picked for the guest (`ipa_id`,
/// `download_url`, `compat`); rows from the plain listings (categories, collections, charts) only name the app,
/// and say nothing about the guest until it is opened.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StoreApp {
    #[serde(default, deserialize_with = "number_or_text")]
    pub ipa_id: i64,
    pub name: String,
    #[serde(default)]
    pub bundle_id: String,
    #[serde(default, deserialize_with = "text_or_number")]
    pub app_store_id: Option<String>,
    pub developer: Option<String>,
    pub version: Option<String>,
    pub min_os: Option<String>,
    pub size: Option<u64>,
    pub icon_url: Option<String>,
    #[serde(default)]
    pub download_url: String,
    pub app_url: Option<String>,
    pub compat: Option<Compat>,
    pub version_count: Option<u32>,
    /// A collection's note on this app.
    pub label: Option<String>,
    /// Listed (in a collection or a chart) but not preserved in the archive: shown greyed out.
    #[serde(skip)]
    pub ghost: bool,
}

fn text_or_number<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) if !s.is_empty() => Some(s),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

fn number_or_text<'de, D: serde::Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::Number(n) => n.as_i64().unwrap_or(0),
        Value::String(s) => s.parse().unwrap_or(0),
        _ => 0,
    })
}

impl StoreApp {
    /// Why it can't be installed on the guest, in plain words, or `None`.
    pub fn incompatibility(&self) -> Option<String> {
        let compat = self.compat.as_ref()?;
        if compat.compatible {
            return None;
        }
        let reason = compat.reasons.first().map(String::as_str).unwrap_or("");
        let text = if reason == "armv6_slice_contains_armv7_code"
            || (reason.starts_with("no_") && reason.ends_with("_slice"))
        {
            "Needs a newer processor"
        } else if let Some(v) = reason.strip_prefix("requires_ios_") {
            return Some(format!("Requires iOS {v}"));
        } else if reason.starts_with("capability:!")
            || reason.ends_with("device_family")
            || reason == "family_not_requested"
        {
            "Not made for this device"
        } else if reason.starts_with("capability:") {
            "Needs hardware this device doesn't have"
        } else {
            match reason {
                "encrypted" => "Encrypted, so it can't run in LightTouch",
                "unavailable" => "Download no longer available",
                "not_analyzed" => "Not checked for compatibility yet",
                _ => "Not compatible with this device",
            }
        };
        Some(text.to_string())
    }

    /// The same app with another archived copy chosen.
    pub fn with_copy(&self, copy: &StoreCopy) -> Self {
        Self { ipa_id: copy.ipa_id, version: Some(copy.version.clone()), size: copy.size, ..self.clone() }
    }
}

/// The copy to select first for a search row that names `ipa_id`.
pub fn preferred(ipa_id: i64) -> i64 {
    BROKEN.iter().find(|(bad, _)| *bad == ipa_id).map(|(_, good)| *good).unwrap_or(ipa_id)
}

/// One archived copy of one version, as the version picker lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreCopy {
    pub ipa_id: i64,
    pub version: String,
    pub size: Option<u64>,
    pub minimum_os: Option<String>,
}

fn encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn get(url: &str) -> Result<ureq::http::Response<ureq::Body>, StoreError> {
    if !url.starts_with("https://") || url[8..].split('/').next().is_none_or(|h| h.is_empty() || h.contains('@')) {
        return fail("The catalog supplied an invalid HTTPS address.");
    }
    let response = agent(Duration::from_secs(30))
        .get(url)
        .call()
        .map_err(|e| StoreError::Failed(format!("Couldn't reach Legacy Store. Check your connection. ({e})")))?;
    let status = response.status().as_u16();
    if status != 200 {
        let body = response.into_body().with_config().limit(4096).read_to_string().unwrap_or_default();
        let message = match status {
            400 if body.contains("device must be one of") => "Legacy Store doesn't support this device yet.".into(),
            503 => "The Internet Archive is busy; try again later.".into(),
            _ => format!("Legacy Store couldn't answer the request (HTTP {status})."),
        };
        return Err(StoreError::Status(status, message));
    }
    Ok(response)
}

fn url(path: &str, query: &[(&str, &str)]) -> String {
    let mut url = format!("{BASE}{path}");
    for (i, (key, value)) in query.iter().enumerate() {
        url.push(if i == 0 { '?' } else { '&' });
        url.push_str(&format!("{}={}", encode(key), encode(value)));
    }
    url
}

/// Browsing answers, kept for ten minutes: the catalog asks to be cached, and tabs get revisited.
static RESPONSES: std::sync::Mutex<Vec<(String, std::time::Instant, Value)>> = std::sync::Mutex::new(Vec::new());
const CACHE_ENTRIES: usize = 64;
const CACHE_TIME: Duration = Duration::from_secs(600);

/// `cached` is for browsing only: what is checked right before a download is always asked for afresh.
fn fetch(url: &str, cached: bool) -> Result<Value, StoreError> {
    if cached {
        let responses = RESPONSES.lock().unwrap();
        if let Some((_, _, value)) = responses.iter().find(|(u, at, _)| u == url && at.elapsed() < CACHE_TIME) {
            return Ok(value.clone());
        }
    }
    let text = get(url)?
        .into_body()
        .with_config()
        .limit(JSON_LIMIT)
        .read_to_string()
        .map_err(|_| StoreError::Failed("Legacy Store sent a response that's too large or unreadable.".into()))?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|_| StoreError::Failed("Legacy Store sent an unreadable response.".into()))?;
    if cached {
        let mut responses = RESPONSES.lock().unwrap();
        responses.retain(|(u, at, _)| u != url && at.elapsed() < CACHE_TIME);
        if responses.len() >= CACHE_ENTRIES {
            responses.remove(0);
        }
        responses.push((url.to_string(), std::time::Instant::now(), value.clone()));
    }
    Ok(value)
}

fn json(path: &str, query: &[(&str, &str)]) -> Result<Value, StoreError> {
    fetch(&url(path, query), false)
}

fn json_cached(path: &str, query: &[(&str, &str)]) -> Result<Value, StoreError> {
    fetch(&url(path, query), true)
}

fn apps(query: &[(&str, &str)], target: &Target) -> Result<Vec<StoreApp>, StoreError> {
    let mut query = query.to_vec();
    query.push(("device", &target.product_type));
    query.push(("os", &target.ios));
    let value = json("/api/emulator/apps", &query)?;
    let list = value.get("apps").and_then(Value::as_array).filter(|l| l.len() <= 500);
    let Some(list) = list else { return fail("Legacy Store sent an unreadable app list.") };
    list.iter()
        .map(|entry| {
            serde_json::from_value::<StoreApp>(entry.clone())
                .ok()
                .filter(|a| a.ipa_id > 0 && !a.name.is_empty() && !a.download_url.is_empty())
                .ok_or_else(|| StoreError::Failed("Legacy Store sent an unreadable app entry.".into()))
        })
        .collect()
}

/// An empty query lists featured apps that run on the guest; a query also includes ones that don't.
pub fn search(query: &str, target: &Target) -> Result<Vec<StoreApp>, StoreError> {
    let query = query.trim();
    if query.is_empty() {
        apps(&[("limit", "50")], target)
    } else {
        apps(&[("limit", "50"), ("q", query), ("incompatible", "include")], target)
    }
}

/// The catalog's current row for one copy, which must still be compatible.
pub fn compatible_copy(ipa_id: i64, target: &Target) -> Result<StoreApp, StoreError> {
    let id = ipa_id.to_string();
    let found = apps(&[("ipa_id", &id)], target)?;
    let [app] = found.as_slice() else { return fail("This copy is no longer available.") };
    if app.ipa_id != ipa_id {
        return fail("This copy is no longer available.");
    }
    if let Some(problem) = app.incompatibility() {
        return fail(problem);
    }
    Ok(app.clone())
}

fn architectures_run(list: Option<&Value>, target: &Target) -> bool {
    list.and_then(Value::as_array).is_some_and(|a| a.iter().filter_map(Value::as_str).any(|arch| target.runs(arch)))
}

fn text(value: &Value, key: &str) -> Option<String> {
    match value.get(key)? {
        Value::String(s) if !s.is_empty() && s != "null" => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// How the catalog addresses an app: its App Store id, or its bundle id (which the catalog redirects), escaped
/// for a path.
pub fn app_key(app: &StoreApp) -> Option<String> {
    let from_url = app.app_url.as_deref().map(|u| u.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string());
    [app.app_store_id.clone(), Some(app.bundle_id.clone()), from_url]
        .into_iter()
        .flatten()
        .find(|k| !k.is_empty())
        .map(|k| encode(&k))
}

/// Every archived copy of the app that could run on the guest, newest first. The copy the search row named is
/// always kept so the picker can show it selected, unless it is broken.
pub fn versions(app: &StoreApp, target: &Target) -> Result<Vec<StoreCopy>, StoreError> {
    let Some(key) = app_key(app) else { return fail("This app has no catalog identifier.") };
    let value = json_cached(&format!("/api/v1/apps/{key}/versions"), &[])?;
    let Some(data) = value.get("data").and_then(Value::as_array) else {
        return fail("Legacy Store sent unreadable version information.");
    };
    Ok(runnable_copies(data, app.ipa_id, target))
}

/// The copies in a catalog version list that run on the guest, plus the one named `chosen` (if any) either way.
fn runnable_copies(data: &[Value], chosen: i64, target: &Target) -> Vec<StoreCopy> {
    let mut rows = Vec::new();
    for entry in data {
        let minimum = text(entry, "minimum_os_version");
        for copy in entry.get("copies").and_then(Value::as_array).into_iter().flatten() {
            let Some(id) = text(copy, "ipa_id").and_then(|s| s.parse::<i64>().ok()) else { continue };
            if BROKEN.iter().any(|(bad, _)| *bad == id) {
                continue;
            }
            let macho_minimum = text(copy, "macho_min_os");
            let fits = copy.get("install_status").and_then(Value::as_str) == Some("installable")
                && architectures_run(copy.get("architectures"), target)
                && [&minimum, &macho_minimum].into_iter().flatten().all(|m| !requires_newer(m, target));
            if fits || id == chosen {
                let minimum_os = [&minimum, &macho_minimum]
                    .into_iter()
                    .flatten()
                    .max_by_key(|m| version(m).unwrap_or([0, 0, 0]))
                    .cloned();
                rows.push(StoreCopy {
                    ipa_id: id,
                    version: text(entry, "version").unwrap_or_else(|| "Unknown".into()),
                    size: copy.get("size").and_then(Value::as_u64).filter(|s| *s > 0),
                    minimum_os,
                });
            }
        }
    }
    rows
}

/// Identity, availability, processor, device family and minimum iOS of a copy's details.
fn check_copy(details: &Value, app: &StoreApp, target: &Target) -> Result<(), StoreError> {
    if text(details, "ipa_id") != Some(app.ipa_id.to_string())
        || text(details, "bundle_id").unwrap_or_default() != app.bundle_id
    {
        return fail("The archived copy no longer matches this app.");
    }
    if details.get("available").and_then(Value::as_bool) != Some(true) {
        return fail("This archived download is no longer available.");
    }
    let Some(binary) = details.get("binary").filter(|b| b.is_object()) else {
        return fail("This copy hasn't been checked for compatibility.");
    };
    if binary.get("install_status").and_then(Value::as_str) != Some("installable") {
        return fail("This copy is encrypted or hasn't been classified as installable.");
    }
    if !architectures_run(binary.get("architectures"), target)
        || (target.arch == "armv6" && binary.get("armv7_code").and_then(Value::as_bool) == Some(true))
    {
        return fail("This copy needs a newer processor than this device has.");
    }
    if let Some(families) = binary.get("device_family_macho").and_then(Value::as_array) {
        let families: Vec<String> =
            families.iter().map(|f| f.as_str().map(str::to_string).unwrap_or_else(|| f.to_string())).collect();
        if !families.is_empty() && !families.contains(&target.family) {
            return fail("This copy isn't made for this device.");
        }
    }
    for minimum in [app.min_os.clone(), text(binary, "macho_min_os")].into_iter().flatten() {
        if requires_newer(&minimum, target) {
            return fail(format!("Requires iOS {minimum}; this device runs iOS {}.", target.ios));
        }
    }
    Ok(())
}

/// Download one revalidated copy into a new folder under `scratch`, returning the IPA. The caller deletes the
/// folder after installing. `progress(done, total)` returns false to cancel.
pub fn download(
    app: &StoreApp,
    target: &Target,
    scratch: &Path,
    progress: &mut dyn FnMut(u64, Option<u64>) -> bool,
) -> Result<PathBuf, StoreError> {
    let current = compatible_copy(app.ipa_id, target)?;
    if current.bundle_id != app.bundle_id {
        return fail("The archived copy no longer matches this app.");
    }
    let details = json(&format!("/api/v1/copies/{}", current.ipa_id), &[])?;
    check_copy(&details, &current, target)?;
    let checksum = text(&details, "md5");
    if checksum.as_ref().is_some_and(|c| c.len() != 32 || !c.bytes().all(|b| b.is_ascii_hexdigit())) {
        return fail("The archive supplied an invalid file checksum.");
    }
    let size = details.get("size").and_then(Value::as_u64);
    if size.is_some_and(|s| s == 0 || s > DOWNLOAD_LIMIT) {
        return fail("The archive supplied an invalid file size.");
    }
    let dir = scratch.join(format!("store-{}-{}", current.ipa_id, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| StoreError::Failed(format!("Couldn't create a download folder: {e}")))?;
    let result = (|| {
        let partial = dir.join("download.partial");
        let response = get(&current.download_url)?;
        let mut body = response.into_body().into_reader();
        let mut out = std::fs::File::create(&partial).map_err(|e| StoreError::Failed(e.to_string()))?;
        let mut digest = md5::Context::new();
        let mut buffer = vec![0u8; 1 << 16];
        let mut count = 0u64;
        loop {
            let read = body.read(&mut buffer).map_err(|e| StoreError::Failed(format!("Download failed: {e}")))?;
            if read == 0 {
                break;
            }
            count += read as u64;
            if count > DOWNLOAD_LIMIT || size.is_some_and(|s| count > s) {
                return fail("The download is larger than the archive says it is.");
            }
            out.write_all(&buffer[..read]).map_err(|e| StoreError::Failed(e.to_string()))?;
            digest.consume(&buffer[..read]);
            if !progress(count, size) {
                return Err(StoreError::Cancelled);
            }
        }
        out.sync_all().map_err(|e| StoreError::Failed(e.to_string()))?;
        if count == 0 || size.is_some_and(|s| count != s) {
            return fail("The download is incomplete or differs from the archive's size.");
        }
        if let Some(checksum) = &checksum {
            if format!("{:x}", digest.finalize()) != checksum.to_lowercase() {
                return fail("The downloaded IPA failed its checksum check.");
            }
        }
        let ipa = dir.join("App.ipa");
        std::fs::rename(&partial, &ipa).map_err(|e| StoreError::Failed(e.to_string()))?;
        Ok(ipa)
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    result
}

/// An app icon's bytes (PNG), or `None` if it can't be fetched.
pub fn icon(url: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    get(url).ok()?.into_body().into_reader().take(1 << 20).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// Version numbers as the Legacy Store and apps write them: up to three numbers, the first not zero.
fn parse_version(text: &str) -> Option<[u32; 3]> {
    version(text.trim()).filter(|v| v[0] != 0)
}

/// The highest of the version strings that read as versions, as the catalog wrote it (a note after a space is
/// ignored for comparing).
pub fn newest_version<'a>(versions: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    versions
        .into_iter()
        .filter_map(|text| parse_version(text.split(' ').next().unwrap_or("")).map(|v| (v, text)))
        .fold(None, |best: Option<([u32; 3], &str)>, next| match best {
            Some(b) if b.0 >= next.0 => Some(b),
            _ => Some(next),
        })
        .map(|(_, text)| text)
}

/// `newest` when it is newer than the `installed` version, else None: a version that doesn't read as one never
/// claims an update.
pub fn update_to<'a>(installed: Option<&str>, newest: Option<&'a str>) -> Option<&'a str> {
    let have = parse_version(installed?)?;
    let want = parse_version(newest?.split(' ').next().unwrap_or(""))?;
    (want > have).then_some(newest?)
}

/// Whether version `a` is older than `b` (both as the catalog writes them); unreadable ones aren't.
pub fn older(a: &str, b: &str) -> bool {
    match (parse_version(a.split(' ').next().unwrap_or("")), parse_version(b)) {
        (Some(a), Some(b)) => a < b,
        _ => false,
    }
}

/// A category (the API calls it a genre).
#[derive(Debug, Clone, PartialEq)]
pub struct Genre {
    pub id: String,
    pub name: String,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Collection {
    pub slug: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub count: u64,
}

/// App rows from a listing; `next` is the address of the following page, which only the paged listings have.
#[derive(Debug, Clone, Default)]
pub struct Page {
    pub rows: Vec<StoreApp>,
    pub total: u64,
    pub next: Option<String>,
}

/// One chart position. `app` is None for an app the archive has not preserved.
#[derive(Debug, Clone)]
pub struct ChartRow {
    pub position: u32,
    pub name: String,
    pub developer: Option<String>,
    pub app: Option<StoreApp>,
}

/// One archived chart snapshot: `date` is the snapshot nearest the one asked for, `dates` every snapshot of it.
#[derive(Debug, Clone)]
pub struct Chart {
    pub title: String,
    pub date: String,
    pub dates: Vec<String>,
    pub rows: Vec<ChartRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Shot {
    pub url: String,
    pub width: u32,
    pub height: u32,
}

/// What the catalog says about an app beyond its listing row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AppInfo {
    pub developer: Option<String>,
    pub genre: Option<String>,
    pub released: Option<String>,
}

/// An installed app the catalog knows: a listing row for its page, and the newest version that runs on the guest.
#[derive(Debug, Clone)]
pub struct Known {
    pub app: StoreApp,
    pub newest: Option<String>,
}

/// The charts the website offers, as the API names them (slug, title).
pub const CHART_TYPES: &[(&str, &str)] = &[
    ("top-free", "Top Free"),
    ("top-paid", "Top Paid"),
    ("top-grossing", "Top Grossing"),
    ("new-noteworthy", "New & Noteworthy"),
    ("new-free", "New Free"),
    ("new-paid", "New Paid"),
];

/// How the app lists can be ordered (the API's sort values).
pub const CATEGORY_SORTS: &[(&str, &str)] =
    &[("versions", "Most archived"), ("newest", "Newest"), ("first_date", "Oldest"), ("name", "A to Z")];

fn rows(list: Option<&Value>, what: &str) -> Result<Vec<StoreApp>, StoreError> {
    let Some(list) = list.and_then(Value::as_array).filter(|l| l.len() <= 500) else {
        return fail(format!("Legacy Store sent an unreadable {what}."));
    };
    list.iter()
        .map(|entry| {
            serde_json::from_value::<StoreApp>(entry.clone())
                .ok()
                .filter(|a| !a.name.is_empty())
                .ok_or_else(|| StoreError::Failed("Legacy Store sent an unreadable app entry.".into()))
        })
        .collect()
}

/// The categories that have apps, A to Z.
pub fn genres() -> Result<Vec<Genre>, StoreError> {
    let value = json_cached("/api/v1/genres", &[])?;
    let Some(data) = value.get("data").and_then(Value::as_array) else {
        return fail("Legacy Store sent an unreadable category list.");
    };
    let mut list: Vec<Genre> = data
        .iter()
        .map(|g| Genre {
            id: text(g, "genre_id").unwrap_or_default(),
            name: text(g, "name").unwrap_or_default(),
            count: g.get("app_count").and_then(Value::as_u64).unwrap_or(0),
        })
        .filter(|g| !g.id.is_empty() && !g.name.is_empty() && g.count > 0)
        .collect();
    list.sort_by_key(|g| g.name.to_lowercase());
    Ok(list)
}

/// The first page of a category's apps, in a `CATEGORY_SORTS` order; `page` follows `Page::next`.
pub fn genre_apps(genre: &str, sort: &str) -> Result<Page, StoreError> {
    page(&url("/api/v1/apps", &[("genre", genre), ("sort", sort), ("limit", "50")]))
}

/// A page of a paged listing, from the address the catalog gave for it. Only the catalog's own addresses are
/// followed.
pub fn page(address: &str) -> Result<Page, StoreError> {
    if !address.starts_with(&format!("{BASE}/")) {
        return fail("The catalog supplied an unexpected address.");
    }
    let body = fetch(address, true)?;
    Ok(Page {
        rows: rows(body.get("data"), "app list")?,
        total: body.get("total").and_then(Value::as_u64).unwrap_or(0),
        next: text(&body, "next_url"),
    })
}

pub fn collections() -> Result<Vec<Collection>, StoreError> {
    let value = json_cached("/api/v1/collections", &[])?;
    let Some(data) = value.get("data").and_then(Value::as_array) else {
        return fail("Legacy Store sent an unreadable collection list.");
    };
    Ok(data
        .iter()
        .map(|c| Collection {
            slug: text(c, "slug").unwrap_or_default(),
            title: text(c, "title").unwrap_or_default(),
            subtitle: text(c, "subtitle").or_else(|| text(c, "series")),
            count: c.get("app_count").and_then(Value::as_u64).unwrap_or(0),
        })
        .filter(|c| !c.slug.is_empty() && !c.title.is_empty())
        .collect())
}

/// A collection's apps in the editors' order; a member's note, if it has one, rides along as `label`.
pub fn collection(slug: &str) -> Result<Page, StoreError> {
    let value = json_cached(&format!("/api/v1/collections/{}", encode(slug)), &[])?;
    let Some(members) = value.get("members").and_then(Value::as_array) else {
        return fail("Legacy Store sent an unreadable collection.");
    };
    let mut apps = Vec::new();
    for member in members.iter().take(500) {
        let label = text(member, "label");
        match member.get("app").filter(|a| a.is_object()) {
            Some(app) => {
                let mut app: StoreApp = serde_json::from_value(app.clone())
                    .ok()
                    .filter(|a: &StoreApp| !a.name.is_empty())
                    .ok_or_else(|| StoreError::Failed("Legacy Store sent an unreadable app entry.".into()))?;
                app.label = label.or(app.label);
                apps.push(app);
            }
            // A member the archive has not preserved has only its label, and is listed greyed out.
            None => {
                if let Some(name) = label {
                    apps.push(StoreApp { name, ghost: true, ..Default::default() });
                }
            }
        }
    }
    Ok(Page { total: apps.len() as u64, rows: apps, next: None })
}

/// One archived chart. `device` is "iphone" or "ipad"; `date` (YYYY-MM-DD) picks the snapshot nearest it, the
/// newest when None.
pub fn chart(kind: &str, device: &str, genre: Option<&str>, date: Option<&str>) -> Result<Chart, StoreError> {
    let mut query = vec![("type", kind), ("device", device)];
    if let Some(genre) = genre {
        query.push(("genre", genre));
    }
    if let Some(date) = date {
        query.push(("date", date));
    }
    let body = json_cached("/api/v1/charts", &query).map_err(|e| match e {
        // Most type, category and device combinations were never captured.
        StoreError::Status(404, _) => StoreError::Status(404, "No chart was archived for this selection.".into()),
        other => other,
    })?;
    let Some(positions) = body.get("positions").and_then(Value::as_array) else {
        return fail("Legacy Store sent an unreadable chart.");
    };
    let dates = body
        .get("available_dates")
        .and_then(Value::as_array)
        .map(|l| l.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default();
    Ok(Chart {
        title: body.get("chart_type").and_then(|t| text(t, "name")).unwrap_or_else(|| kind.to_string()),
        date: text(&body, "snapshot_date").unwrap_or_default(),
        dates,
        rows: positions
            .iter()
            .take(500)
            .map(|p| ChartRow {
                position: p.get("position").and_then(Value::as_u64).unwrap_or(0) as u32,
                name: text(p, "name").unwrap_or_default(),
                developer: text(p, "developer"),
                app: p
                    .get("app")
                    .filter(|a| a.is_object())
                    .and_then(|a| serde_json::from_value::<StoreApp>(a.clone()).ok())
                    .filter(|a| !a.name.is_empty()),
            })
            .collect(),
    })
}

/// Category, first release and developer, beyond the listing row.
pub fn app_info(app: &StoreApp) -> Result<AppInfo, StoreError> {
    let Some(key) = app_key(app) else { return fail("This app has no catalog identifier.") };
    let body = json_cached(&format!("/api/v1/apps/{key}"), &[])?;
    Ok(AppInfo {
        developer: text(&body, "developer"),
        genre: text(&body, "genre"),
        released: text(&body, "original_release_date").map(|d| d.chars().take(4).collect()),
    })
}

/// The newest archived set of screenshots (the catalog has no app descriptions).
pub fn screenshots(app: &StoreApp) -> Result<Vec<Shot>, StoreError> {
    let Some(key) = app_key(app) else { return Ok(Vec::new()) };
    let body = json_cached(&format!("/api/v1/apps/{key}/screenshots"), &[])?;
    let shots = body
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|set| set.get("shots").and_then(Value::as_array))
        .find(|s| !s.is_empty());
    Ok(shots
        .into_iter()
        .flatten()
        .filter_map(|s| {
            Some(Shot {
                url: text(s, "url")?,
                width: s.get("width").and_then(Value::as_u64).unwrap_or(0) as u32,
                height: s.get("height").and_then(Value::as_u64).unwrap_or(0) as u32,
            })
        })
        .collect())
}

/// The catalog's entry for a bundle id (the catalog redirects the bundle id to the app), or None when it has none.
/// One cached request: the app's own answer lists its versions. Another identity coming back is "none".
pub fn lookup(bundle_id: &str, target: &Target) -> Result<Option<Known>, StoreError> {
    let body = match json_cached(&format!("/api/v1/apps/{}", encode(bundle_id)), &[]) {
        Ok(body) => body,
        Err(StoreError::Status(404, _)) => return Ok(None),
        Err(e) => return Err(e),
    };
    if !text(&body, "bundle_id").is_some_and(|b| b.eq_ignore_ascii_case(bundle_id)) {
        return Ok(None);
    }
    let app = StoreApp {
        bundle_id: text(&body, "bundle_id").unwrap_or_default(),
        name: text(&body, "name").or_else(|| text(&body, "display_name")).unwrap_or_else(|| bundle_id.to_string()),
        app_store_id: text(&body, "app_store_id"),
        developer: text(&body, "developer"),
        icon_url: text(&body, "icon_url"),
        ..Default::default()
    };
    let copies = body.get("versions").and_then(Value::as_array).map(|v| runnable_copies(v, 0, target));
    let newest = copies.as_deref().and_then(|c| newest_version(c.iter().map(|c| c.version.as_str())));
    Ok(Some(Known { newest: newest.map(str::to_string), app }))
}

/// An image's bytes (a screenshot), or None if it can't be fetched.
pub fn image(url: &str) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    get(url).ok()?.into_body().into_reader().take(4 << 20).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_is_by_number_not_by_text() {
        assert_eq!(newest_version(["1.9", "1.10", "1.2"]), Some("1.10"));
        assert_eq!(newest_version(["Unknown", "1.0", "2.0"]), Some("2.0"));
        assert_eq!(newest_version([]), None);
        assert_eq!(newest_version(["Unknown"]), None);
    }

    #[test]
    fn an_update_is_only_claimed_for_a_version_that_is_really_newer() {
        assert_eq!(update_to(Some("1.26"), Some("1.27")), Some("1.27"));
        assert_eq!(update_to(Some("2.0"), Some("2.0.1")), Some("2.0.1"));
        assert_eq!(update_to(Some("1.27"), Some("1.27")), None);
        assert_eq!(update_to(Some("1.28"), Some("1.27")), None);
        assert_eq!(update_to(Some("1.0"), Some("1.0.0")), None);
        // Unreadable on either side: say nothing rather than guess.
        assert_eq!(update_to(None, Some("1.27")), None);
        assert_eq!(update_to(Some("1.2.3.4"), Some("1.27")), None);
        assert_eq!(update_to(Some("1.0"), Some("Unknown")), None);
    }

    #[test]
    fn listing_rows_read_without_a_copy() {
        let row: StoreApp = serde_json::from_str(
            r#"{"app_store_id":"343200656","bundle_id":"com.clickgamer.AngryBirds","name":"Angry Birds",
                "developer":null,"version_count":73,"icon_url":"https://legacystore.app/icon/x"}"#,
        )
        .unwrap();
        assert_eq!((row.ipa_id, row.app_store_id.as_deref()), (0, Some("343200656")));
        assert_eq!(app_key(&row).as_deref(), Some("343200656"));
        let search: StoreApp =
            serde_json::from_str(r#"{"ipa_id":"93216","name":"A","bundle_id":"b","download_url":"u"}"#).unwrap();
        assert_eq!(search.ipa_id, 93216);
    }

    /// Against the live catalog: cargo test -p lighttouch-catalog live_browsing -- --ignored
    #[test]
    #[ignore]
    fn live_browsing() {
        let target = Target::of("iPod4,1", "6.1.6").unwrap();
        let genres = genres().unwrap();
        assert!(genres.iter().any(|g| g.name == "Games"), "{genres:?}");
        let games = genres.iter().find(|g| g.name == "Games").unwrap();
        let first = genre_apps(&games.id, "versions").unwrap();
        assert!(!first.rows.is_empty() && first.next.is_some());
        let second = page(first.next.as_deref().unwrap()).unwrap();
        assert!(!second.rows.is_empty());
        let featured = collection("featured").unwrap();
        assert!(featured.rows.iter().any(|a| a.name == "Angry Birds"));
        let chart = chart("top-free", "iphone", None, Some("2010-07-01")).unwrap();
        assert!(chart.rows.len() > 50 && chart.dates.len() > 10, "{}", chart.rows.len());
        let birds = &featured.rows.iter().find(|a| a.name == "Angry Birds").unwrap();
        assert!(!screenshots(birds).unwrap().is_empty());
        assert_eq!(app_info(birds).unwrap().genre.as_deref(), Some("Games"));
        let copies = versions(birds, &target).unwrap();
        assert!(!copies.is_empty());
        let known = lookup("com.clickgamer.AngryBirds", &target).unwrap().unwrap();
        assert!(known.newest.is_some(), "{known:?}");
        assert!(lookup("com.example.nothing.here", &target).unwrap().is_none());
    }
}
