//! Guest networking, as Android's Settings → Network: off by default; Direct internet or the Wayback Machine at a
//! chosen calendar day, and local network access as an explicit opt-in. Every setting applies to the running session:
//! the engine forwards the proxy address from boot, and the worker makes the proxy the first time networking is on.
//!
//! Besides the settings this holds the worker's side, kept free of the engine so it can be tested: `NetworkSession`
//! (saved preferences versus what the running session applied) and `configure_guest` (installing the proxy's
//! certificate and Wi-Fi proxy through the guest tools, retrying only requests the guest refused before admission).
use lighttouch_core::network::{NetworkConfig, valid_date};
use lighttouch_core::protocol::NetworkStatus;
use lighttouch_core::runtime::NetworkMode;
use lighttouch_core::sys;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    #[default]
    Direct,
    Archive,
}

impl Route {
    pub fn label(self) -> &'static str {
        match self {
            Self::Direct => "Direct internet",
            Self::Archive => "Wayback Machine",
        }
    }
}

/// A calendar day, independent of any timezone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Day {
    pub year: i32,
    pub month: u32,
    pub day: u32,
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

impl Day {
    pub const FIRST: Day = Day { year: 1996, month: 1, day: 1 };

    /// Today on the host's calendar.
    pub fn today() -> Self {
        let (year, month, day) = lighttouch_core::local_date();
        Self { year, month, day }
    }

    /// Strict `yyyyMMdd`.
    pub fn parse(text: &str) -> Option<Self> {
        if text.len() != 8 || !text.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let day = Self { year: text[..4].parse().ok()?, month: text[4..6].parse().ok()?, day: text[6..].parse().ok()? };
        (day.month >= 1 && day.month <= 12 && day.day >= 1 && day.day <= days_in_month(day.year, day.month))
            .then_some(day)
    }

    pub fn compact(self) -> String {
        format!("{:04}{:02}{:02}", self.year, self.month, self.day)
    }

    /// "9 September 2009".
    pub fn long(self) -> String {
        format!("{} {} {}", self.day, month_name(self.month), self.year)
    }

    /// "9 Sep 2009".
    pub fn short(self) -> String {
        format!("{} {} {}", self.day, &month_name(self.month)[..3], self.year)
    }

    /// The nearest real day from 1996 through `today`: the day of the month shrinks to fit (31 March → 30 April).
    pub fn clamped(year: i32, month: u32, day: u32, today: Day) -> Self {
        let year = year.clamp(Self::FIRST.year, today.year.max(Self::FIRST.year));
        let month = month.clamp(1, 12);
        let day = day.clamp(1, days_in_month(year, month));
        Self { year, month, day }.max(Self::FIRST).min(today.max(Self::FIRST))
    }
}

pub fn month_name(month: u32) -> &'static str {
    MONTHS[(month.clamp(1, 12) - 1) as usize]
}

pub fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Settings → Network, saved with the other settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkSettings {
    pub enabled: bool,
    pub route: Route,
    /// `yyyyMMdd`, from 1996 through today.
    pub archive_date: String,
    /// Reach other devices on the local network (Direct internet only).
    pub local_network: bool,
}

impl Default for NetworkSettings {
    fn default() -> Self {
        Self { enabled: false, route: Route::Direct, archive_date: "20090909".into(), local_network: false }
    }
}

impl NetworkSettings {
    pub fn mode(&self) -> NetworkMode {
        match (self.enabled, self.route) {
            (false, _) => NetworkMode::Off,
            (true, Route::Direct) => NetworkMode::Direct,
            (true, Route::Archive) => NetworkMode::Archive,
        }
    }

    /// What the proxy and the engine apply.
    pub fn config(&self) -> NetworkConfig {
        NetworkConfig { mode: self.mode(), archive_date: self.archive_date.clone(), local_network: self.local_network }
    }

    pub fn date(&self) -> Day {
        Day::parse(&self.archive_date).unwrap_or(Day::FIRST)
    }

    /// The archive date is a real day from 1996 through `today`.
    pub fn validate(&self, today: Day) -> Result<(), String> {
        match Day::parse(&self.archive_date) {
            Some(day) if day >= Day::FIRST && day <= today && valid_date(&self.archive_date) => Ok(()),
            _ => Err("Choose an archive date between 1996 and today.".into()),
        }
    }

    /// "Off", "Direct internet", "Wayback · 9 Sep 2009".
    pub fn summary(&self) -> String {
        match self.mode() {
            NetworkMode::Off => "Off".into(),
            NetworkMode::Direct => Route::Direct.label().into(),
            NetworkMode::Archive => format!("Wayback · {}", self.date().short()),
        }
    }

    /// Saved settings that don't validate turn networking off (the date resets too).
    pub fn sanitized(self, today: Day) -> Self {
        if self.validate(today).is_ok() { self } else { Self::default() }
    }
}

/// Settings are read leniently: a network section that doesn't parse is networking off, not lost settings.
pub(crate) fn lenient<'de, D: serde::Deserializer<'de>>(d: D) -> Result<NetworkSettings, D::Error> {
    let value = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

/// The running session's side: saved preferences and what the worker applied are distinct. A live change is retried
/// while the engine is starting (`LT_AGAIN`/`LT_BUSY`), then applied once or reported; it never touches the guest.
#[derive(Debug, Default)]
pub struct NetworkSession {
    endpoint: bool,
    active: NetworkConfig,
    requested: NetworkConfig,
    pending: bool,
    notice: String,
}

impl NetworkSession {
    /// At boot: `endpoint` says whether the engine forwards to a proxy socket (only an engine that can't leaves a
    /// session that must restart to go online).
    pub fn begin(config: NetworkConfig, endpoint: bool) -> Self {
        Self { endpoint, active: config.clone(), requested: config, pending: false, notice: String::new() }
    }

    pub fn request(&mut self, config: NetworkConfig) {
        self.pending = config != self.active;
        self.notice = if self.pending { "Applying network settings…".into() } else { String::new() };
        self.requested = config;
    }

    pub fn pending(&self) -> bool {
        self.pending
    }

    /// Try a pending change. `apply` changes the engine's policy and the proxy (making it on the first change that
    /// needs one); its error carries an `lt_status`.
    pub fn refresh(&mut self, apply: impl FnOnce(&NetworkConfig) -> Result<(), lighttouch_core::Error>) {
        if !self.pending {
            return;
        }
        if !self.endpoint {
            if self.requested.mode != NetworkMode::Off {
                self.notice = "Saved. Stop and start the device to enable networking for this session.".into();
            } else {
                self.active = self.requested.clone();
                self.notice.clear();
            }
            self.pending = false;
            return;
        }
        match apply(&self.requested) {
            Ok(()) => {
                self.active = self.requested.clone();
                self.pending = false;
                self.notice.clear();
            }
            Err(e) if e.is(sys::LT_AGAIN) || e.is(sys::LT_BUSY) => {
                self.notice = "Waiting for the device to apply network settings…".into();
            }
            Err(e) => {
                self.pending = false;
                self.notice = format!("Couldn't apply network settings: {e}. Saved for next start.");
            }
        }
    }

    pub fn endpoint(&self) -> bool {
        self.endpoint
    }

    pub fn active(&self) -> &NetworkConfig {
        &self.active
    }

    pub fn notice(&self) -> &str {
        &self.notice
    }
}

/// What the guest-tools agent does with a typed request (the worker's runtime, or a fake in tests).
pub trait GuestAgent {
    /// Submit; `LT_BUSY`/`LT_AGAIN` mean it wasn't admitted.
    fn request(&self, operation: u32, argument: &str, timeout_ms: u32) -> lighttouch_core::Result<u64>;
    /// (state, guest status, output): state 1 queued, 2 complete, 3 rejected, 4 cancelled, 5 timed out.
    fn poll(&self, id: u64) -> lighttouch_core::Result<(u32, i32, String)>;
    fn cancel(&self, id: u64);
    /// The session still runs.
    fn active(&self) -> bool;
    fn now_ms(&self) -> u64;
    fn sleep_ms(&self, ms: u64);
}

/// Run one guest operation. Retries only admission refusals (busy guest tools, an agent that is still starting) for
/// up to `admission_ms`; once admitted it is polled to the end and never resubmitted, so a lost or timed-out
/// mutation is reported rather than replayed.
pub fn guest_operation(
    agent: &dyn GuestAgent,
    operation: u32,
    argument: &str,
    admission_ms: u64,
) -> Result<String, String> {
    let admission_deadline = agent.now_ms() + admission_ms;
    let id = loop {
        if !agent.active() {
            return Err("Network setup stopped".into());
        }
        match agent.request(operation, argument, 20_000) {
            Ok(id) if id != 0 => break id,
            Err(e) if (e.is(sys::LT_BUSY) || e.is(sys::LT_AGAIN)) && agent.now_ms() < admission_deadline => {}
            Err(e) if e.is(sys::LT_BUSY) || e.is(sys::LT_AGAIN) => {
                return Err(if operation == sys::LT_GUEST_PING {
                    "The guest tools didn't start. Update guest tools and restart the device.".into()
                } else {
                    format!("The guest tools stayed busy: {e}")
                });
            }
            Err(e) => return Err(e.to_string()),
            Ok(_) => return Err("The guest tools didn't accept the request".into()),
        }
        agent.sleep_ms(100);
    };
    let result = (|| {
        let completion_deadline = agent.now_ms() + 25_000;
        while agent.active() {
            let (state, status, output) = agent.poll(id).map_err(|e| e.to_string())?;
            match state {
                1 if agent.now_ms() < completion_deadline => agent.sleep_ms(100),
                1 => return Err("Guest network setup timed out; restart the device to try again".into()),
                2 if status == 0 => return Ok(output),
                2 => return Err(format!("Guest network setup step {operation} failed ({status})")),
                _ => {
                    return Err(format!(
                        "Guest network setup step {operation} failed ({status}). Update guest tools and restart."
                    ));
                }
            }
        }
        Err("Network setup stopped".into())
    })();
    agent.cancel(id);
    result
}

pub const HELPERS: [(&str, u32); 2] =
    [("ittrust", sys::LT_GUEST_NETWORK_TRUST_CHUNK), ("itproxy", sys::LT_GUEST_NETWORK_ROUTE_CHUNK)];

/// Install the proxy's CA and the Wi-Fi proxy in the guest once its tools answer, as Android does: upload the
/// bundled `ittrust` and `itproxy` (in 2 KiB chunks the guest assembles atomically, so packages that predate them
/// work), install the certificate, trust it, and point the Wi-Fi proxy at the host. `report` gets each step.
pub fn configure_guest(
    agent: &dyn GuestAgent,
    helpers: &Path,
    certificate: &[u8],
    report: &dyn Fn(&str),
) -> Result<(), String> {
    use lighttouch_core::network::base64;
    // Before waiting on the guest: a build without the helpers can't finish.
    let mut binaries = Vec::new();
    for (name, operation) in HELPERS {
        match std::fs::read(helpers.join(name)) {
            Ok(binary) if !binary.is_empty() && binary.len() <= 1 << 20 => binaries.push((binary, operation)),
            Ok(_) => return Err(format!("The bundled network helper {name} is invalid.")),
            Err(_) => return Err("This build doesn't include the guest network helpers (ittrust, itproxy).".into()),
        }
    }
    report("Waiting for guest tools…");
    guest_operation(agent, sys::LT_GUEST_PING, "", 120_000)?;
    report("Preparing guest network tools…");
    for (binary, operation) in &binaries {
        let mut offset = 0;
        while offset < binary.len() {
            let end = (offset + 2048).min(binary.len());
            let last = if end == binary.len() { 1 } else { 0 };
            let argument = format!("{offset} {last} {}", base64(&binary[offset..end]));
            guest_operation(agent, *operation, &argument, 20_000)?;
            offset = end;
        }
    }
    report("Installing guest certificate…");
    guest_operation(agent, sys::LT_GUEST_NETWORK_CERTIFICATE, &base64(certificate), 20_000)?;
    let trusted = guest_operation(agent, sys::LT_GUEST_NETWORK_TRUST, "", 20_000)?;
    if !trusted.contains("Guest trust add: 0") {
        return Err("The guest did not accept its HTTPS certificate. Update guest tools and restart.".into());
    }
    report("Configuring the guest connection…");
    if !guest_operation(agent, sys::LT_GUEST_NETWORK_ROUTE, "", 20_000)?.contains("Proxy enabled") {
        return Err("The guest did not accept the proxy. Update guest tools and restart.".into());
    }
    report("Ready");
    Ok(())
}

/// The session's status as the worker reports it, from its parts.
pub fn status(
    session: &NetworkSession,
    setup: (&str, bool),
    activity: Option<lighttouch_core::network::Activity>,
) -> NetworkStatus {
    let activity = activity.unwrap_or_default();
    NetworkStatus {
        proxy: session.endpoint(),
        applied: session.active().clone(),
        setup: setup.0.to_string(),
        setup_error: setup.1,
        activity: activity.summary,
        connections: activity.connections,
        completed: activity.completed,
        failed: activity.failed,
        last_result: activity.last_result,
        last_result_error: activity.last_result_error,
        notice: session.notice().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lighttouch_core::Error;
    use std::cell::{Cell, RefCell};

    const TODAY: Day = Day { year: 2026, month: 10, day: 26 };

    fn archive(date: &str) -> NetworkSettings {
        NetworkSettings { enabled: true, route: Route::Archive, archive_date: date.into(), local_network: false }
    }

    #[test]
    fn archive_dates_are_strict_calendar_days_from_1996_through_today() {
        for ok in ["19960101", "20000229", "20090909", "20261026"] {
            assert!(archive(ok).validate(TODAY).is_ok(), "{ok}");
        }
        for bad in [
            "19951231",
            "20090230",
            "19000229",
            "20261027",
            "20270101",
            "2009090",
            "200909090",
            "2009-9-9",
            "abcdefgh",
            "",
        ] {
            assert!(archive(bad).validate(TODAY).is_err(), "{bad}");
        }
    }

    #[test]
    fn off_by_default_and_invalid_turns_off() {
        let default = NetworkSettings::default();
        assert_eq!(default.mode(), NetworkMode::Off);
        assert!(!default.local_network);
        let tomorrow = archive("20261027").sanitized(TODAY);
        assert_eq!(tomorrow, NetworkSettings::default());
        let fine = archive("20090909").sanitized(TODAY);
        assert_eq!(fine.mode(), NetworkMode::Archive);
    }

    #[test]
    fn settings_files_fail_closed() {
        let path = std::env::temp_dir().join(format!("lt-network-settings-{}.json", std::process::id()));
        let load = |text: &str| {
            std::fs::write(&path, text).unwrap();
            crate::settings::Settings::load(&path)
        };
        // A bad date, and a section of the wrong shape: networking off, the other settings kept.
        let bad_date =
            load(r#"{"tilt": false, "network": {"enabled": true, "route": "archive", "archive_date": "20090230"}}"#);
        assert_eq!(bad_date.network, NetworkSettings::default());
        assert!(!bad_date.tilt);
        let wrong = load(r#"{"tilt": false, "network": {"enabled": "yes"}}"#);
        assert_eq!(wrong.network, NetworkSettings::default());
        assert!(!wrong.tilt);
        let good = load(r#"{"network": {"enabled": true, "route": "direct", "local_network": true}}"#);
        assert_eq!(good.network.mode(), NetworkMode::Direct);
        assert!(good.network.local_network);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn day_helpers() {
        let day = Day::parse("20090909").unwrap();
        assert_eq!(day.long(), "9 September 2009");
        assert_eq!(day.short(), "9 Sep 2009");
        assert_eq!(day.compact(), "20090909");
        assert_eq!(Day::clamped(2009, 4, 31, TODAY), Day { year: 2009, month: 4, day: 30 });
        assert_eq!(Day::clamped(2028, 12, 31, TODAY), TODAY);
        assert_eq!(Day::clamped(1990, 5, 5, TODAY), Day { year: 1996, month: 5, day: 5 });
        assert_eq!(archive("20090909").summary(), "Wayback · 9 Sep 2009");
    }

    fn direct() -> NetworkConfig {
        NetworkConfig { mode: NetworkMode::Direct, ..Default::default() }
    }

    #[test]
    fn offline_session_turns_on_live() {
        let mut session = NetworkSession::begin(NetworkConfig::default(), true);
        session.request(direct());
        let mut applied = None;
        session.refresh(|config| {
            applied = Some(config.mode);
            Ok(())
        });
        assert_eq!(applied, Some(NetworkMode::Direct));
        assert_eq!(session.active().mode, NetworkMode::Direct);
        assert_eq!(session.notice(), "");
    }

    #[test]
    fn session_without_an_endpoint_needs_a_restart_to_enable() {
        let mut session = NetworkSession::begin(NetworkConfig::default(), false);
        session.request(direct());
        session.refresh(|_| panic!("no proxy to apply to"));
        assert!(session.notice().contains("Stop and start"));
        assert_eq!(session.active().mode, NetworkMode::Off);
        // Back to off: nothing pending, no notice.
        session.request(NetworkConfig::default());
        session.refresh(|_| panic!("no proxy to apply to"));
        assert_eq!(session.notice(), "");
    }

    #[test]
    fn live_changes_retry_admission_and_keep_the_active_route_on_failure() {
        let mut session = NetworkSession::begin(direct(), true);
        let archive = NetworkConfig { mode: NetworkMode::Archive, ..Default::default() };
        session.request(archive.clone());
        session.refresh(|_| Err(Error::new(sys::LT_AGAIN, "starting")));
        assert!(session.pending() && session.notice().contains("Waiting"));
        assert_eq!(session.active().mode, NetworkMode::Direct);
        session.refresh(|_| Ok(()));
        assert!(!session.pending());
        assert_eq!(session.active(), &archive);
        // Disable and re-enable live.
        session.request(NetworkConfig::default());
        session.refresh(|_| Ok(()));
        assert_eq!(session.active().mode, NetworkMode::Off);
        session.request(direct());
        session.refresh(|_| Err(Error::new(sys::LT_UNSUPPORTED, "Engine cannot change network policy live")));
        assert!(!session.pending() && session.notice().contains("Saved for next start"));
        assert_eq!(session.active().mode, NetworkMode::Off);
    }

    /// A scripted guest: request results in order, then each poll's state.
    struct Fake {
        admissions: RefCell<Vec<Result<u64, Error>>>,
        polls: RefCell<Vec<(u32, i32, String)>>,
        requests: RefCell<Vec<(u32, String)>>,
        cancelled: RefCell<Vec<u64>>,
        clock: Cell<u64>,
    }

    impl Fake {
        fn new(admissions: Vec<Result<u64, Error>>, polls: Vec<(u32, i32, &str)>) -> Self {
            Self {
                admissions: RefCell::new(admissions),
                polls: RefCell::new(polls.into_iter().map(|(a, b, c)| (a, b, c.to_string())).collect()),
                requests: Default::default(),
                cancelled: Default::default(),
                clock: Cell::new(0),
            }
        }
    }

    impl GuestAgent for Fake {
        fn request(&self, operation: u32, argument: &str, _timeout_ms: u32) -> lighttouch_core::Result<u64> {
            self.requests.borrow_mut().push((operation, argument.to_string()));
            let mut a = self.admissions.borrow_mut();
            if a.is_empty() { Ok(self.requests.borrow().len() as u64) } else { a.remove(0) }
        }
        fn poll(&self, _id: u64) -> lighttouch_core::Result<(u32, i32, String)> {
            let mut p = self.polls.borrow_mut();
            Ok(if p.is_empty() { (2, 0, String::new()) } else { p.remove(0) })
        }
        fn cancel(&self, id: u64) {
            self.cancelled.borrow_mut().push(id);
        }
        fn active(&self) -> bool {
            true
        }
        fn now_ms(&self) -> u64 {
            self.clock.get()
        }
        fn sleep_ms(&self, ms: u64) {
            self.clock.set(self.clock.get() + ms);
        }
    }

    #[test]
    fn admission_refusals_are_retried_but_accepted_mutations_never_resubmitted() {
        let busy = || Err(Error::new(sys::LT_BUSY, "guest operation already pending"));
        let fake = Fake::new(
            vec![busy(), Err(Error::new(sys::LT_AGAIN, "agent starting")), Ok(7)],
            vec![(1, 0, ""), (2, 0, "ok")],
        );
        assert_eq!(guest_operation(&fake, sys::LT_GUEST_NETWORK_TRUST, "", 20_000).unwrap(), "ok");
        assert_eq!(fake.requests.borrow().len(), 3);
        assert_eq!(*fake.cancelled.borrow(), vec![7]);

        // Timed out after admission: reported, not replayed.
        let lost = Fake::new(vec![Ok(9)], vec![(5, 0, "")]);
        assert!(guest_operation(&lost, sys::LT_GUEST_NETWORK_ROUTE, "", 20_000).is_err());
        assert_eq!(lost.requests.borrow().len(), 1);

        // Still queued past the completion deadline: also not resubmitted.
        let stuck = Fake::new(vec![Ok(3)], vec![(1, 0, ""); 400]);
        assert!(guest_operation(&stuck, sys::LT_GUEST_NETWORK_ROUTE, "", 20_000).unwrap_err().contains("timed out"));
        assert_eq!(stuck.requests.borrow().len(), 1);

        // Invalid requests fail at once.
        let invalid = Fake::new(vec![Err(Error::new(sys::LT_INVALID, "bad chunk"))], vec![]);
        assert!(guest_operation(&invalid, sys::LT_GUEST_NETWORK_TRUST_CHUNK, "x", 20_000).is_err());
        assert_eq!(invalid.requests.borrow().len(), 1);

        // Admission gives up after its window.
        let absent = Fake::new(vec![Err(Error::new(sys::LT_AGAIN, "agent absent")); 1000], vec![]);
        assert!(guest_operation(&absent, sys::LT_GUEST_PING, "", 1_000).unwrap_err().contains("didn't start"));
    }

    #[test]
    fn guest_setup_uploads_helpers_in_chunks_then_installs() {
        let dir = std::env::temp_dir().join(format!("lt-network-helpers-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ittrust"), vec![1u8; 5000]).unwrap();
        std::fs::write(dir.join("itproxy"), vec![2u8; 100]).unwrap();
        let fake = Fake::new(
            vec![],
            vec![
                (2, 0, ""),
                (2, 0, ""),
                (2, 0, ""),
                (2, 0, ""),
                (2, 0, ""),
                (2, 0, ""),
                (2, 0, "Guest trust add: 0"),
                (2, 0, "Proxy enabled"),
            ],
        );
        let steps = RefCell::new(Vec::new());
        configure_guest(&fake, &dir, b"cert", &|s| steps.borrow_mut().push(s.to_string())).unwrap();
        let requests = fake.requests.borrow();
        let kinds: Vec<u32> = requests.iter().map(|r| r.0).collect();
        assert_eq!(kinds, vec![0, 9, 9, 9, 10, 6, 7, 8]);
        assert!(requests[1].1.starts_with("0 0 "));
        assert!(requests[2].1.starts_with("2048 0 "));
        assert!(requests[3].1.starts_with("4096 1 "));
        assert!(requests[4].1.starts_with("0 1 "));
        assert_eq!(requests[5].1, base64_of(b"cert"));
        assert_eq!(steps.borrow().last().unwrap(), "Ready");

        // A build without the helpers says so before waiting on the guest.
        let empty = Fake::new(vec![], vec![]);
        let missing = configure_guest(&empty, &dir.join("nothing"), b"cert", &|_| {});
        assert!(missing.unwrap_err().contains("doesn't include"));
        assert!(empty.requests.borrow().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn base64_of(bytes: &[u8]) -> String {
        lighttouch_core::network::base64(bytes)
    }
}
