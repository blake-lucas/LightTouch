//! Legacy Store and app installs for the current device, independent of any UI toolkit. Network and guest
//! service work runs on plain threads and reports back through a channel that `tick` applies; a generation number
//! per kind of request drops answers that a newer request has replaced. Mirrors the Android store: the device's
//! own list and search, the website's browsing tabs, the apps on the device with newer versions, and an install
//! queue that downloads and installs one app after another.
use crate::Waker;
use crate::queue::{self, InstallQueue, QueueItem, QueueState, Report};
use lighttouch_catalog::store::{
    self, AppInfo, Chart, Collection, Genre, Known, Page, Shot, StoreApp, StoreCopy, StoreError,
};
use lighttouch_catalog::{Catalog, Target, ipa};
use lighttouch_services::apps::{Guest, InstallProgress};
use lighttouch_services::instproxy::InstalledApp;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// An app icon or screenshot, fetched once.
#[derive(Debug, Clone)]
pub enum Icon {
    Loading,
    Ready(Arc<[u8]>),
    Missing,
}

/// The sections the store can be browsed by: the website's Featured, Charts, Categories and Collections, after the
/// device's own list and its apps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Device,
    Installed,
    Featured,
    Charts,
    Categories,
    Collections,
}

impl Tab {
    pub const ALL: [Tab; 6] =
        [Tab::Device, Tab::Installed, Tab::Featured, Tab::Charts, Tab::Categories, Tab::Collections];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Device => "For this device",
            Tab::Installed => "Installed",
            Tab::Featured => "Featured",
            Tab::Charts => "Charts",
            Tab::Categories => "Categories",
            Tab::Collections => "Collections",
        }
    }
}

/// A list of apps that loads in pages (or all at once), for one section of the store.
#[derive(Debug, Default)]
pub struct Listing {
    pub rows: Vec<StoreApp>,
    pub next: Option<String>,
    pub total: u64,
    pub loading: bool,
    pub error: Option<String>,
    pub loaded: bool,
    generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListingKind {
    Featured,
    Category,
    Collection,
}

/// What the catalog said about one installed app. A lookup that failed is not kept as an answer: it is asked again.
#[derive(Debug, Clone)]
pub enum Lookup {
    Found(Known),
    Missing,
}

/// What an installed app calls its version, which is what the store's version numbers match.
pub fn app_version(app: &InstalledApp) -> Option<&str> {
    app.short_version.as_deref().or(app.version.as_deref()).filter(|v| !v.is_empty())
}

pub fn app_title(app: &InstalledApp) -> &str {
    app.name.as_deref().filter(|n| !n.is_empty()).unwrap_or(&app.bundle_id)
}

/// An app to install: one from the catalog (its checked copy), or an IPA file the user chose.
#[derive(Debug, Clone)]
pub enum InstallRequest {
    Catalog(StoreApp),
    Local(PathBuf),
}

/// The apps the running guest has, and what the Legacy Store knows of each (its page, its icon, a newer version
/// that runs here). The catalog is asked once per app, one request at a time and cached, and stops at the first
/// failure rather than retrying every app.
#[derive(Debug, Default)]
pub struct Installed {
    pub list: Option<Vec<InstalledApp>>,
    pub loading: bool,
    pub error: Option<String>,
    pub lookups: HashMap<String, Lookup>,
    pub lookup_error: Option<String>,
    /// An installed app the catalog doesn't know (or hasn't answered for yet), shown on a page of its own.
    pub plain: Option<InstalledApp>,
    looking: bool,
    lookup_generation: u64,
    refresh_generation: u64,
    refreshed: Option<Instant>,
}

impl Installed {
    /// The newest version that runs here, if it is newer than what `app` has.
    pub fn update(&self, app: &InstalledApp) -> Option<String> {
        match self.lookups.get(&app.bundle_id) {
            Some(Lookup::Found(known)) => {
                store::update_to(app_version(app), known.newest.as_deref()).map(str::to_string)
            }
            _ => None,
        }
    }

    pub fn known(&self, bundle_id: &str) -> Option<&Known> {
        match self.lookups.get(bundle_id) {
            Some(Lookup::Found(known)) => Some(known),
            _ => None,
        }
    }
}

/// The store's browsing tabs. They come from the catalog's plain listings, which say nothing about the device.
#[derive(Debug)]
pub struct Browser {
    pub featured: Listing,
    pub chart_type: String,
    chart_device_choice: Option<String>,
    pub chart_genre: Option<Genre>,
    pub chart: Option<Chart>,
    pub chart_loading: bool,
    pub chart_error: Option<String>,
    chart_date: Option<String>,
    chart_generation: u64,
    pub genres: Option<Vec<Genre>>,
    pub genres_error: Option<String>,
    genres_loading: bool,
    pub category: Option<Genre>,
    pub category_sort: String,
    pub category_apps: Listing,
    pub collections: Option<Vec<Collection>>,
    pub collections_error: Option<String>,
    collections_loading: bool,
    pub collection: Option<Collection>,
    pub collection_apps: Listing,
}

impl Default for Browser {
    fn default() -> Self {
        Self {
            featured: Listing::default(),
            chart_type: "top-free".into(),
            chart_device_choice: None,
            chart_genre: None,
            chart: None,
            chart_loading: false,
            chart_error: None,
            chart_date: None,
            chart_generation: 0,
            genres: None,
            genres_error: None,
            genres_loading: false,
            category: None,
            category_sort: "versions".into(),
            category_apps: Listing::default(),
            collections: None,
            collections_error: None,
            collections_loading: false,
            collection: None,
            collection_apps: Listing::default(),
        }
    }
}

enum Message {
    Guest {
        socket: PathBuf,
        result: Result<(Target, Vec<InstalledApp>), String>,
    },
    Results {
        generation: u64,
        result: Result<Vec<StoreApp>, StoreError>,
    },
    Copies {
        generation: u64,
        result: Result<Vec<StoreCopy>, StoreError>,
    },
    Checked {
        generation: u64,
        result: Result<StoreApp, StoreError>,
    },
    Info {
        generation: u64,
        info: AppInfo,
    },
    Shots {
        generation: u64,
        shots: Vec<Shot>,
    },
    Listing {
        kind: ListingKind,
        generation: u64,
        reset: bool,
        result: Result<Page, StoreError>,
    },
    Chart {
        generation: u64,
        result: Result<Chart, StoreError>,
    },
    Genres(Result<Vec<Genre>, StoreError>),
    Collections(Result<Vec<Collection>, StoreError>),
    InstalledList {
        socket: PathBuf,
        generation: u64,
        result: Result<Vec<InstalledApp>, String>,
    },
    Lookup {
        generation: u64,
        bundle_id: String,
        result: Result<Option<Known>, StoreError>,
    },
    LookupsDone {
        generation: u64,
    },
    /// The guest's app list after an install.
    AfterInstall(Vec<InstalledApp>),
    Crash(String),
    Icon {
        url: String,
        bytes: Option<Vec<u8>>,
    },
}

pub struct Store {
    /// The device record the store is for; its installed apps and target are that device's alone.
    record: String,
    pub target: Option<Target>,
    /// The target came from the running guest rather than the record's catalog entry.
    pub confirmed: bool,
    pub tab: Tab,
    pub query: String,
    pub results: Vec<StoreApp>,
    pub searching: bool,
    pub show_incompatible: bool,
    pub message: String,
    /// The app whose page is open.
    pub chosen: Option<StoreApp>,
    pub copies: Option<Vec<StoreCopy>>,
    pub selected: Option<i64>,
    /// Why the chosen copy can't be installed.
    pub problem: Option<String>,
    /// The chosen copy, checked against the guest and ready to install.
    pub candidate: Option<StoreApp>,
    /// What the catalog says about the open app, and its newest screenshots; both arrive after the versions.
    pub info: Option<AppInfo>,
    pub shots: Vec<Shot>,
    /// Bundle id → version on the running guest, for the "Installed" labels.
    pub installed: HashMap<String, String>,
    pub installed_apps: Installed,
    pub browser: Browser,
    /// The install queue as of the last tick.
    pub queue: Vec<QueueItem>,
    pub crash: Option<String>,
    /// Why the running device has no USB link for installs.
    pub usb_problem: Option<String>,
    installer: InstallQueue<InstallRequest>,
    socket: Arc<Mutex<Option<PathBuf>>>,
    icons: HashMap<String, Icon>,
    guest_socket: Option<PathBuf>,
    search_generation: u64,
    check_generation: u64,
    /// The open app page, for its info and screenshots.
    page_generation: u64,
    tx: Sender<Message>,
    rx: Receiver<Message>,
    waker: Waker,
}

fn background(name: &str, work: impl FnOnce() + Send + 'static) {
    let _ = std::thread::Builder::new().name(name.into()).spawn(work);
}

/// Guest service failures in plain words.
fn service_error(e: impl std::fmt::Display) -> String {
    e.to_string()
}

pub fn byte_size(bytes: u64) -> String {
    if bytes >= 1 << 30 {
        format!("{:.1} GB", bytes as f64 / (1u64 << 30) as f64)
    } else if bytes >= 1 << 20 {
        format!("{:.1} MB", bytes as f64 / (1u64 << 20) as f64)
    } else {
        format!("{} KB", bytes / 1024)
    }
}

/// An app's place in the queue: by bundle, since two versions of one app can't install together.
fn queue_key(app: &StoreApp) -> String {
    let key = [Some(app.bundle_id.clone()), app.app_store_id.clone(), store::app_key(app)]
        .into_iter()
        .flatten()
        .find(|k| !k.is_empty())
        .unwrap_or_else(|| app.ipa_id.to_string());
    format!("app:{key}")
}

/// Roughly when the device's iOS was in use, so its charts are of apps it can run.
fn era_date(target: Option<&Target>) -> Option<String> {
    let major: u32 = target?.ios.split('.').next()?.parse().ok()?;
    Some(format!("{}-07-01", (2007 + major).min(2025)))
}

impl Store {
    pub fn new(scratch: PathBuf, waker: Waker) -> Self {
        let (tx, rx) = channel();
        let socket: Arc<Mutex<Option<PathBuf>>> = Arc::default();
        let work = Installer {
            socket: socket.clone(),
            exact: Mutex::new(None),
            scratch,
            tx: tx.clone(),
            waker: waker.clone(),
        };
        let wake = waker.clone();
        Self {
            record: String::new(),
            target: None,
            confirmed: false,
            tab: Tab::Device,
            query: String::new(),
            results: Vec::new(),
            searching: false,
            show_incompatible: false,
            message: String::new(),
            chosen: None,
            copies: None,
            selected: None,
            problem: None,
            candidate: None,
            info: None,
            shots: Vec::new(),
            installed: HashMap::new(),
            installed_apps: Installed::default(),
            browser: Browser::default(),
            queue: Vec::new(),
            crash: None,
            usb_problem: None,
            installer: InstallQueue::new(work, move || wake()),
            socket,
            icons: HashMap::new(),
            guest_socket: None,
            search_generation: 0,
            check_generation: 0,
            page_generation: 0,
            tx,
            rx,
            waker,
        }
    }

    fn sender(&self) -> (Sender<Message>, Waker) {
        (self.tx.clone(), self.waker.clone())
    }

    fn spawn(&self, name: &str, work: impl FnOnce() -> Message + Send + 'static) {
        let (tx, waker) = self.sender();
        background(name, move || {
            let _ = tx.send(work());
            waker();
        });
    }

    /// Apps are waiting, downloading or installing.
    pub fn installing(&self) -> bool {
        self.queue.iter().any(QueueItem::active)
    }

    /// iOS is changing its apps: it shouldn't be stopped until it finishes.
    pub fn mutating(&self) -> bool {
        self.queue.iter().any(|i| i.state == QueueState::Installing && i.committed)
    }

    /// Whether app installs can reach the guest: it has attached over USB and answered.
    pub fn guest_ready(&self) -> bool {
        self.guest_socket.is_some() && self.confirmed
    }

    pub fn running(&self) -> bool {
        self.guest_socket.is_some()
    }

    /// Follow the current device (`record`, with its board and build), its running guest's usbmux socket, and why
    /// there is no USB link when there can't be one (`usb_problem`).
    pub fn follow(&mut self, record: Option<(&str, &str, &str)>, socket: Option<&Path>, usb_problem: Option<&str>) {
        self.usb_problem = usb_problem.map(str::to_string);
        let (id, board, build) = record.unwrap_or(("", "", ""));
        if id != self.record {
            // Another record starts over, even of the same model: two iPod touch 2Gs on different iOS versions have
            // different apps.
            self.record = id.to_string();
            self.installed.clear();
            self.installed_apps = Installed::default();
            self.confirmed = false;
            self.target = None;
            self.results.clear();
            self.chosen = None;
            self.crash = None;
        }
        if !self.confirmed {
            let next = Catalog::get().find_build(board, build).and_then(|f| Target::of(&f.product_type, &f.version));
            if next != self.target {
                self.set_target(next);
            }
        }
        if socket != self.guest_socket.as_deref() {
            self.guest_socket = socket.map(Path::to_path_buf);
            *self.socket.lock().unwrap() = self.guest_socket.clone();
            self.installed.clear();
            self.installed_apps.refresh_generation += 1;
            self.installed_apps.loading = false;
            self.installed_apps.list = None;
            self.installed_apps.error = None;
            self.installed_apps.plain = None;
            self.confirmed = false;
            if let Some(socket) = socket {
                self.refresh_guest(socket.to_path_buf());
            }
        }
    }

    /// Another device (or a guest reporting its exact iOS version) has other apps and other charts.
    fn set_target(&mut self, next: Option<Target>) {
        self.target = next;
        self.results.clear();
        self.chosen = None;
        self.forget_lookups();
        self.reset_browser();
        if self.target.is_some() {
            self.search();
            let tab = self.tab;
            self.show(tab);
        }
    }

    /// Ask the running guest for its exact model and iOS version, and the apps it has.
    fn refresh_guest(&self, socket: PathBuf) {
        self.spawn("store-guest", move || {
            // It attaches once iOS has started, which can take a while on a cold boot.
            let result =
                Guest::connect(&socket, Duration::from_secs(300)).map_err(service_error).and_then(|mut guest| {
                    let target = guest.target().map_err(service_error)?;
                    let apps = guest.installed().map_err(service_error)?;
                    Ok((target, apps))
                });
            Message::Guest { socket, result }
        });
    }

    pub fn tick(&mut self) {
        self.queue = self.installer.snapshot();
        while let Ok(message) = self.rx.try_recv() {
            self.apply(message);
        }
    }

    fn apply(&mut self, message: Message) {
        match message {
            Message::Guest { socket, result } => {
                if self.guest_socket.as_ref() != Some(&socket) {
                    return; // another guest answers by now
                }
                match result {
                    Ok((target, apps)) => {
                        self.confirmed = true;
                        if self.target.as_ref() != Some(&target) {
                            self.set_target(Some(target));
                        }
                        self.set_installed(apps);
                    }
                    Err(e) if !self.installing() => self.message = format!("App services: {e}"),
                    Err(_) => {}
                }
            }
            Message::Results { generation, result } if generation == self.search_generation => {
                self.searching = false;
                match result {
                    Ok(apps) => {
                        if apps.is_empty() {
                            self.message = "No apps found.".into();
                        }
                        self.results = apps;
                    }
                    Err(e) => self.message = e.to_string(),
                }
            }
            Message::Copies { generation, result } if generation == self.check_generation => match result {
                Ok(rows) => {
                    let preferred = self.chosen.as_ref().map(|a| store::preferred(a.ipa_id));
                    let first = rows.iter().find(|c| Some(c.ipa_id) == preferred).or(rows.first()).map(|c| c.ipa_id);
                    if rows.is_empty() {
                        self.problem = Some("No version of this app runs on this device.".into());
                    }
                    self.copies = Some(rows);
                    if let Some(id) = first {
                        self.choose(id);
                    }
                }
                Err(e) => {
                    self.copies = Some(Vec::new());
                    self.problem = Some(format!("Couldn't load versions: {e}"));
                }
            },
            Message::Checked { generation, result } if generation == self.check_generation => match result {
                Ok(app) => self.candidate = Some(app),
                Err(e) => self.problem = Some(e.to_string()),
            },
            Message::Info { generation, info } if generation == self.page_generation => self.info = Some(info),
            Message::Shots { generation, shots } if generation == self.page_generation => self.shots = shots,
            Message::Results { .. }
            | Message::Copies { .. }
            | Message::Checked { .. }
            | Message::Info { .. }
            | Message::Shots { .. } => {}
            Message::Listing { kind, generation, reset, result } => {
                let listing = self.listing_mut(kind);
                if generation != listing.generation {
                    return;
                }
                listing.loading = false;
                match result {
                    Ok(page) => {
                        if reset {
                            listing.rows.clear();
                        }
                        listing.rows.extend(page.rows);
                        listing.next = page.next;
                        listing.total = page.total;
                        listing.loaded = true;
                    }
                    Err(e) => listing.error = Some(e.to_string()),
                }
            }
            Message::Chart { generation, result } if generation == self.browser.chart_generation => {
                self.browser.chart_loading = false;
                match result {
                    Ok(chart) => self.browser.chart = Some(chart),
                    Err(e) => {
                        self.browser.chart = None;
                        self.browser.chart_error = Some(e.to_string());
                    }
                }
            }
            Message::Chart { .. } => {}
            Message::Genres(result) => {
                self.browser.genres_loading = false;
                match result {
                    Ok(list) => self.browser.genres = Some(list),
                    Err(e) => self.browser.genres_error = Some(e.to_string()),
                }
            }
            Message::Collections(result) => {
                self.browser.collections_loading = false;
                match result {
                    Ok(list) => self.browser.collections = Some(list),
                    Err(e) => self.browser.collections_error = Some(e.to_string()),
                }
            }
            Message::InstalledList { socket, generation, result } => {
                if generation != self.installed_apps.refresh_generation || self.guest_socket.as_ref() != Some(&socket) {
                    return;
                }
                self.installed_apps.loading = false;
                match result {
                    Ok(apps) => self.set_installed(apps),
                    Err(e) => self.installed_apps.error = Some(format!("Couldn't read the apps on this device: {e}")),
                }
            }
            Message::Lookup { generation, bundle_id, result }
                if generation == self.installed_apps.lookup_generation =>
            {
                match result {
                    Ok(found) => {
                        let lookup = found.map(Lookup::Found).unwrap_or(Lookup::Missing);
                        // The page of an app the catalog turns out to know becomes its store page.
                        if let (Lookup::Found(known), Some(plain)) = (&lookup, &self.installed_apps.plain) {
                            if plain.bundle_id == bundle_id {
                                let app = known.app.clone();
                                self.installed_apps.plain = None;
                                self.installed_apps.lookups.insert(bundle_id, lookup);
                                self.open(&app);
                                return;
                            }
                        }
                        self.installed_apps.lookups.insert(bundle_id, lookup);
                    }
                    Err(e) => self.installed_apps.lookup_error = Some(e.to_string()),
                }
            }
            Message::LookupsDone { generation } if generation == self.installed_apps.lookup_generation => {
                self.installed_apps.looking = false;
                // Apps installed while the sweep ran; not after a failure, which waits for Retry.
                if self.installed_apps.lookup_error.is_none() {
                    self.look_up();
                }
            }
            Message::Lookup { .. } | Message::LookupsDone { .. } => {}
            Message::AfterInstall(apps) => {
                if self.guest_socket.is_some() {
                    self.set_installed(apps);
                }
            }
            Message::Crash(text) => self.crash = Some(text),
            Message::Icon { url, bytes } => {
                self.icons.insert(url, bytes.map(|b| Icon::Ready(Arc::from(b))).unwrap_or(Icon::Missing));
            }
        }
    }

    fn listing_mut(&mut self, kind: ListingKind) -> &mut Listing {
        match kind {
            ListingKind::Featured => &mut self.browser.featured,
            ListingKind::Category => &mut self.browser.category_apps,
            ListingKind::Collection => &mut self.browser.collection_apps,
        }
    }

    pub fn search(&mut self) {
        let Some(target) = self.target.clone() else {
            self.message = "Choose a device first.".into();
            return;
        };
        self.tab = Tab::Device;
        self.search_generation += 1;
        let (generation, query) = (self.search_generation, self.query.clone());
        self.searching = true;
        self.message.clear();
        self.spawn("store-search", move || Message::Results { generation, result: store::search(&query, &target) });
    }

    /// The results to show: everything, or only what runs here.
    pub fn visible_results(&self) -> Vec<&StoreApp> {
        self.results.iter().filter(|a| self.show_incompatible || a.incompatibility().is_none()).collect()
    }

    /// Open an app's page: its versions that run here, its category and its screenshots.
    pub fn open(&mut self, app: &StoreApp) {
        let Some(target) = self.target.clone() else { return };
        self.chosen = Some(app.clone());
        self.copies = None;
        self.selected = None;
        self.problem = None;
        self.candidate = None;
        self.info = None;
        self.shots.clear();
        self.page_generation += 1;
        let page = self.page_generation;
        let info_app = app.clone();
        self.spawn("store-info", move || Message::Info {
            generation: page,
            info: store::app_info(&info_app).unwrap_or_default(),
        });
        let shots_app = app.clone();
        self.spawn("store-shots", move || Message::Shots {
            generation: page,
            shots: store::screenshots(&shots_app).unwrap_or_default(),
        });
        self.check_generation += 1;
        let generation = self.check_generation;
        let app = app.clone();
        self.spawn("store-versions", move || Message::Copies { generation, result: store::versions(&app, &target) });
    }

    pub fn close(&mut self) {
        self.chosen = None;
        self.check_generation += 1;
        self.page_generation += 1;
    }

    /// Check one archived copy against this device.
    pub fn choose(&mut self, ipa_id: i64) {
        let (Some(target), Some(app)) = (self.target.clone(), self.chosen.clone()) else { return };
        self.selected = Some(ipa_id);
        self.problem = None;
        self.candidate = None;
        self.check_generation += 1;
        let generation = self.check_generation;
        self.spawn("store-copy-check", move || {
            let result = store::compatible_copy(ipa_id, &target).and_then(|found| {
                // Rows from the charts and categories may not name a bundle.
                if !app.bundle_id.is_empty() && found.bundle_id != app.bundle_id {
                    Err(StoreError::Failed("This copy belongs to a different app.".into()))
                } else {
                    Ok(found)
                }
            });
            Message::Checked { generation, result }
        });
    }

    /// The app's queue entry, until it is dismissed.
    pub fn queued(&self, app: &StoreApp) -> Option<&QueueItem> {
        let key = queue_key(app);
        self.queue.iter().find(|i| i.key == key)
    }

    /// The queue entry of an installed app, by bundle id.
    pub fn queued_bundle(&self, bundle_id: &str) -> Option<&QueueItem> {
        let key = format!("app:{bundle_id}");
        self.queue.iter().find(|i| i.key == key)
    }

    fn enqueue(&mut self, key: String, name: &str, request: InstallRequest) {
        if self.guest_socket.is_none() {
            self.message = "Start the device to install apps.".into();
            return;
        }
        self.installer.enqueue(&key, name, request);
        self.queue = self.installer.snapshot();
    }

    /// Queue the checked candidate, and go back to where the app was found.
    pub fn install_candidate(&mut self) {
        let Some(app) = self.candidate.clone() else { return };
        let key = self.chosen.as_ref().map(queue_key).unwrap_or_else(|| queue_key(&app));
        self.enqueue(key, &app.name.clone(), InstallRequest::Catalog(app));
        self.close();
    }

    /// Queue an IPA file the user chose.
    pub fn install_file(&mut self, path: PathBuf) {
        let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "IPA".into());
        self.enqueue(format!("local:{}", path.display()), &name, InstallRequest::Local(path));
    }

    pub fn cancel(&self, id: u64) {
        self.installer.cancel(id);
    }

    pub fn retry(&self, id: u64) {
        self.installer.retry(id);
    }

    pub fn dismiss(&self, id: u64) {
        self.installer.dismiss(id);
    }

    pub fn clear_finished(&self) {
        self.installer.clear_finished();
    }

    /// Read why the last app quit: the useful lines of the guest's newest crash report.
    pub fn show_last_crash(&mut self) {
        let Some(socket) = self.guest_socket.clone() else {
            self.crash = Some("Start the device to read its crash reports.".into());
            return;
        };
        self.crash = Some("Reading the device's crash reports…".into());
        self.spawn("store-crash", move || {
            let text = match Guest::connect(&socket, Duration::from_secs(30)).and_then(|mut g| g.last_crash()) {
                Ok(Some((name, body))) => format!("{name}\n\n{}", crash_summary(&body)),
                Ok(None) => "The device has no crash reports.".into(),
                Err(e) => format!("Couldn't read crash reports: {e}"),
            };
            Message::Crash(text)
        });
    }

    pub fn dismiss_crash(&mut self) {
        self.crash = None;
    }

    /// An app icon or screenshot, fetched in the background the first time it is asked for.
    pub fn icon(&mut self, url: &str) -> Icon {
        if let Some(icon) = self.icons.get(url) {
            return icon.clone();
        }
        self.icons.insert(url.to_string(), Icon::Loading);
        let url = url.to_string();
        self.spawn("store-icon", move || {
            let bytes = store::image(&url);
            Message::Icon { url, bytes }
        });
        Icon::Loading
    }

    // ---- The apps on the device.

    /// The guest's current list, from any source.
    fn set_installed(&mut self, apps: Vec<InstalledApp>) {
        let mut apps: Vec<InstalledApp> = apps.into_iter().filter(|a| !a.bundle_id.is_empty()).collect();
        apps.sort_by_key(|a| app_title(a).to_lowercase());
        self.installed =
            apps.iter().map(|a| (a.bundle_id.clone(), app_version(a).unwrap_or("unknown").to_string())).collect();
        self.installed_apps.refreshed = Some(Instant::now());
        self.installed_apps.list = Some(apps);
        self.installed_apps.error = None;
        self.look_up();
    }

    /// Read the guest's list again; a recent one stands (the queue brings a fresh one after each install).
    pub fn refresh_installed(&mut self, force: bool) {
        let Some(socket) = self.guest_socket.clone() else { return };
        let fresh = self.installed_apps.refreshed.is_some_and(|t| t.elapsed() < Duration::from_secs(10));
        if self.installed_apps.loading || (!force && self.installed_apps.list.is_some() && fresh) {
            self.look_up();
            return;
        }
        self.installed_apps.refresh_generation += 1;
        let generation = self.installed_apps.refresh_generation;
        self.installed_apps.loading = true;
        self.installed_apps.error = None;
        self.spawn("store-installed", move || {
            let result =
                Guest::connect(&socket, Duration::from_secs(30)).and_then(|mut g| g.installed()).map_err(service_error);
            Message::InstalledList { socket, generation, result }
        });
    }

    /// Ask the catalog about the apps it hasn't answered for; needs the guest's own device type, which decides
    /// what runs.
    pub fn look_up(&mut self) {
        let Some(target) = self.target.clone() else { return };
        let Some(list) = &self.installed_apps.list else { return };
        let pending: Vec<String> = list
            .iter()
            .map(|a| a.bundle_id.clone())
            .filter(|id| !self.installed_apps.lookups.contains_key(id))
            .collect();
        if !self.confirmed || pending.is_empty() || self.installed_apps.looking {
            return;
        }
        self.installed_apps.lookup_generation += 1;
        let generation = self.installed_apps.lookup_generation;
        self.installed_apps.lookup_error = None;
        self.installed_apps.looking = true;
        let (tx, waker) = self.sender();
        background("store-lookup", move || {
            for bundle_id in pending {
                let result = store::lookup(&bundle_id, &target);
                let failed = result.is_err();
                if tx.send(Message::Lookup { generation, bundle_id, result }).is_err() {
                    return;
                }
                waker();
                if failed {
                    break;
                }
            }
            let _ = tx.send(Message::LookupsDone { generation });
            waker();
        });
    }

    /// What the catalog knew changes with the device (what runs on it).
    fn forget_lookups(&mut self) {
        self.installed_apps.lookup_generation += 1;
        self.installed_apps.lookups.clear();
        self.installed_apps.lookup_error = None;
        self.installed_apps.looking = false;
    }

    /// An installed app's page: the store's when the catalog knows the app, else what the device says about it.
    pub fn open_installed(&mut self, app: &InstalledApp) {
        match self.installed_apps.known(&app.bundle_id).map(|k| k.app.clone()) {
            Some(found) => self.open(&found),
            None => self.installed_apps.plain = Some(app.clone()),
        }
    }

    pub fn close_plain(&mut self) {
        self.installed_apps.plain = None;
    }

    // ---- Browsing.

    /// Another device record: what was loaded for the last one (its era's charts, its apps) is not this one's.
    fn reset_browser(&mut self) {
        let b = &mut self.browser;
        for listing in [&mut b.featured, &mut b.category_apps, &mut b.collection_apps] {
            let generation = listing.generation + 1;
            *listing = Listing { generation, ..Default::default() };
        }
        b.chart_generation += 1;
        b.chart = None;
        b.chart_date = None;
        b.chart_error = None;
        b.chart_loading = false;
        b.chart_genre = None;
        b.chart_device_choice = None;
        b.category = None;
        b.collection = None;
    }

    /// iPad targets see the iPad charts unless the user picks otherwise.
    pub fn chart_device(&self) -> &str {
        match &self.browser.chart_device_choice {
            Some(choice) => choice,
            None if self.target.as_ref().is_some_and(|t| t.family == "2") => "ipad",
            None => "iphone",
        }
    }

    /// Switch to a tab and load what it shows, once.
    pub fn show(&mut self, tab: Tab) {
        self.tab = tab;
        match tab {
            Tab::Device => {}
            Tab::Installed => self.refresh_installed(false),
            Tab::Featured => {
                if !self.browser.featured.loaded && !self.browser.featured.loading {
                    self.load_featured();
                }
            }
            Tab::Charts => {
                self.load_genres();
                let b = &self.browser;
                if b.chart.is_none() && !b.chart_loading && b.chart_error.is_none() {
                    let date = b.chart_date.clone().or_else(|| era_date(self.target.as_ref()));
                    self.load_chart(date);
                }
            }
            Tab::Categories => self.load_genres(),
            Tab::Collections => {
                if self.browser.collections.is_none() {
                    self.load_collections();
                }
            }
        }
    }

    /// Inside a category or collection: Back steps out of it first.
    pub fn can_go_back(&self) -> bool {
        (self.tab == Tab::Categories && self.browser.category.is_some())
            || (self.tab == Tab::Collections && self.browser.collection.is_some())
    }

    pub fn back(&mut self) {
        if self.tab == Tab::Categories {
            self.browser.category = None;
        } else {
            self.browser.collection = None;
        }
    }

    fn load(
        &mut self,
        kind: ListingKind,
        reset: bool,
        fetch: impl FnOnce() -> Result<Page, StoreError> + Send + 'static,
    ) {
        let listing = self.listing_mut(kind);
        if listing.loading && !reset {
            return;
        }
        listing.generation += 1;
        let generation = listing.generation;
        if reset {
            listing.rows.clear();
            listing.next = None;
            listing.total = 0;
            listing.loaded = false;
        }
        listing.loading = true;
        listing.error = None;
        self.spawn("store-browse", move || Message::Listing { kind, generation, reset, result: fetch() });
    }

    /// The next page of a listing that has one.
    pub fn more(&mut self, kind: ListingKind) {
        let listing = self.listing_mut(kind);
        if let (Some(address), false) = (listing.next.clone(), listing.loading) {
            self.load(kind, false, move || store::page(&address));
        }
    }

    pub fn load_featured(&mut self) {
        self.load(ListingKind::Featured, true, || store::collection("featured"));
    }

    pub fn load_chart(&mut self, date: Option<String>) {
        self.browser.chart_date = date.clone();
        self.browser.chart_generation += 1;
        let generation = self.browser.chart_generation;
        self.browser.chart_loading = true;
        self.browser.chart_error = None;
        let kind = self.browser.chart_type.clone();
        let device = self.chart_device().to_string();
        let genre = self.browser.chart_genre.as_ref().map(|g| g.id.clone());
        self.spawn("store-chart", move || Message::Chart {
            generation,
            result: store::chart(&kind, &device, genre.as_deref(), date.as_deref()),
        });
    }

    pub fn reload_chart(&mut self) {
        let date = self.browser.chart_date.clone();
        self.load_chart(date);
    }

    pub fn pick_chart_type(&mut self, slug: &str) {
        self.browser.chart_type = slug.into();
        self.reload_chart();
    }

    pub fn pick_chart_device(&mut self, device: &str) {
        self.browser.chart_device_choice = Some(device.into());
        self.reload_chart();
    }

    pub fn pick_chart_genre(&mut self, genre: Option<Genre>) {
        self.browser.chart_genre = genre;
        self.reload_chart();
    }

    pub fn pick_chart_year(&mut self, year: &str) {
        self.load_chart(Some(format!("{year}-07-01")));
    }

    /// The previous (-1) or next (+1) archived snapshot of this chart.
    pub fn shift_chart(&mut self, delta: isize) {
        let Some(chart) = &self.browser.chart else { return };
        let Some(index) = chart.dates.iter().position(|d| *d == chart.date) else { return };
        if let Some(date) = index.checked_add_signed(delta).and_then(|i| chart.dates.get(i)).cloned() {
            self.load_chart(Some(date));
        }
    }

    pub fn load_genres(&mut self) {
        if self.browser.genres.is_some() || self.browser.genres_loading {
            return;
        }
        self.browser.genres_error = None;
        self.browser.genres_loading = true;
        self.spawn("store-genres", || Message::Genres(store::genres()));
    }

    pub fn open_category(&mut self, genre: Genre) {
        self.browser.category = Some(genre);
        self.load_category();
    }

    pub fn pick_category_sort(&mut self, sort: &str) {
        self.browser.category_sort = sort.into();
        self.load_category();
    }

    fn load_category(&mut self) {
        let Some(genre) = self.browser.category.clone() else { return };
        let sort = self.browser.category_sort.clone();
        self.load(ListingKind::Category, true, move || store::genre_apps(&genre.id, &sort));
    }

    pub fn load_collections(&mut self) {
        if self.browser.collections_loading {
            return;
        }
        self.browser.collections_error = None;
        self.browser.collections_loading = true;
        self.spawn("store-collections", || Message::Collections(store::collections()));
    }

    pub fn open_collection(&mut self, collection: Collection) {
        let slug = collection.slug.clone();
        self.browser.collection = Some(collection);
        self.load(ListingKind::Collection, true, move || store::collection(&slug));
    }

    /// Load a listing's first page again (after an error).
    pub fn retry_listing(&mut self, kind: ListingKind) {
        match kind {
            ListingKind::Featured => self.load_featured(),
            ListingKind::Category => self.load_category(),
            ListingKind::Collection => {
                if let Some(collection) = self.browser.collection.clone() {
                    self.open_collection(collection);
                }
            }
        }
    }
}

/// The queue's two steps against the running guest.
struct Installer {
    socket: Arc<Mutex<Option<PathBuf>>>,
    /// What the running guest is, asked once per guest.
    exact: Mutex<Option<(PathBuf, Target)>>,
    scratch: PathBuf,
    tx: Sender<Message>,
    waker: Waker,
}

impl Installer {
    fn guest(&self, stopped: &str) -> Result<(Guest, Target), String> {
        let socket = self.socket.lock().unwrap().clone().ok_or(stopped)?;
        let mut guest = Guest::connect(&socket, Duration::from_secs(30)).map_err(service_error)?;
        let mut exact = self.exact.lock().unwrap();
        let target = match exact.as_ref().filter(|(s, _)| *s == socket) {
            Some((_, target)) => target.clone(),
            None => {
                let target = guest.target().map_err(service_error)?;
                *exact = Some((socket, target.clone()));
                target
            }
        };
        Ok((guest, target))
    }
}

impl queue::Work<InstallRequest> for Installer {
    fn fetch(
        &self,
        payload: &InstallRequest,
        report: &dyn Report,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<PathBuf, String> {
        report.update("Preparing", Some(0.0));
        match payload {
            InstallRequest::Catalog(app) => {
                let (_, target) = self.guest("Start the device to install apps.")?;
                let mut reported: Option<Instant> = None;
                store::download(app, &target, &self.scratch, &mut |done, total| {
                    if reported.is_none_or(|t| t.elapsed() >= Duration::from_millis(100)) || Some(done) == total {
                        reported = Some(Instant::now());
                        let text = match total {
                            Some(total) => format!("Downloading {} of {}", byte_size(done), byte_size(total)),
                            None => format!("Downloading {}", byte_size(done)),
                        };
                        let fraction = total.map(|t| done as f32 / t.max(1) as f32);
                        report.update(&text, fraction);
                        if let Some(f) = fraction {
                            report.overall(queue::stages::download(f));
                        }
                    }
                    !cancelled()
                })
                .map_err(|e| e.to_string())
            }
            InstallRequest::Local(path) => {
                report.update("Copying", None);
                let dir = self.scratch.join(format!("local-{:x}", lighttouch_core::monotonic_ns()));
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let copy = dir.join("Source.ipa");
                std::fs::copy(path, &copy).map(|_| copy).map_err(|e| {
                    let _ = std::fs::remove_dir_all(&dir);
                    format!("Couldn't read the file: {e}")
                })
            }
        }
    }

    fn install(
        &self,
        payload: &InstallRequest,
        file: &PathBuf,
        report: &dyn Report,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<(), String> {
        let (mut guest, target) = self.guest("The device stopped. Start it to install apps.")?;
        report.update("Checking the app", None);
        let prepared = file.with_file_name("Install.ipa");
        let info = ipa::prepare(file, &prepared, &target).map_err(|e| e.to_string())?;
        if matches!(payload, InstallRequest::Local(_)) {
            report.rename(&info.name);
        }
        if cancelled() {
            return Err("Cancelled".into());
        }
        let result = guest.install(&prepared, &info.bundle_id, Some(info.unpacked), &mut |progress| {
            match progress {
                InstallProgress::Uploading { sent, total } => {
                    let fraction = sent as f32 / total.max(1) as f32;
                    report.update("Copying to the device", Some(fraction));
                    report.overall(queue::stages::copy(fraction));
                    return !cancelled();
                }
                InstallProgress::Submitted => {
                    report.commit();
                    report.update("Installing", None);
                }
                InstallProgress::Installing { status, percent } => {
                    report.commit();
                    let unpacking = status == "ExtractingPackage";
                    report
                        .update(if unpacking { "Unpacking" } else { "Installing" }, percent.map(|p| p as f32 / 100.0));
                    if let (Some(percent), false) = (percent, unpacking) {
                        report.overall(queue::stages::ios_percent(percent));
                    }
                }
                // iOS's own steps are internal names; unpacking, most of a large app's time, shows how much has landed.
                InstallProgress::Unpacking { done, total } => {
                    report.commit();
                    let fraction = done as f32 / total.max(1) as f32;
                    report.update(&format!("Unpacking · {} of {}", byte_size(done), byte_size(total)), Some(fraction));
                    report.overall(queue::stages::unpack(fraction));
                }
            }
            true
        });
        let _ = std::fs::remove_file(&prepared);
        result.map_err(service_error)?;
        if let Ok(apps) = guest.installed() {
            let _ = self.tx.send(Message::AfterInstall(apps));
            (self.waker)();
        }
        Ok(())
    }

    fn discard(&self, file: &PathBuf) {
        // Each fetched file has a folder of its own under the scratch folder.
        if let Some(dir) = file.parent().filter(|d| d.starts_with(&self.scratch) && *d != self.scratch) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// The lines of a crash report that say why an app quit.
fn crash_summary(body: &str) -> String {
    const KEEP: &[&str] = &[
        "Process",
        "Date/Time",
        "OS Version",
        "Exception Type",
        "Exception Codes",
        "Crashed Thread",
        "Dyld Error Message",
        "  Symbol not found",
        "  Referenced from",
        "  Expected in",
        "Termination Reason",
        "Application Specific Information",
    ];
    let lines: Vec<&str> = body.lines().collect();
    let dyld = lines.iter().position(|l| l.starts_with("Dyld Error Message"));
    let mut kept: Vec<&str> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let near_dyld = dyld.is_some_and(|d| i > d && i <= d + 3);
        if (KEEP.iter().any(|k| line.starts_with(k)) || near_dyld) && !kept.contains(line) {
            kept.push(line);
        }
    }
    if kept.is_empty() { body.chars().take(1500).collect() } else { kept.join("\n") }
}
