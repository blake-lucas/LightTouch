//! The worker protocol: one JSON object per line. Commands go to the worker's stdin, events come from its stdout,
//! and its stderr is the session log. Frames travel separately through shared memory (`frames`).
use crate::network::NetworkConfig;
use crate::runtime::{BoardInfo, Contact, State};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// How the UI starts a worker: `lighttouch worker <json WorkerArgs>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerArgs {
    pub library: PathBuf,
    pub record: String,
    pub engine: PathBuf,
    /// Shared-memory segment the UI created for frames.
    pub frames: String,
    /// Cold boot state that was not shut down cleanly.
    #[serde(default)]
    pub recover: bool,
    #[serde(default = "yes")]
    pub audio: bool,
    #[serde(default)]
    pub headphones: bool,
    /// The guest's USB link for app services; `None` boots without a cable.
    #[serde(default)]
    pub usb: Option<UsbArgs>,
    /// Guest networking; `None` boots offline and reports nothing about it.
    #[serde(default)]
    pub network: Option<NetworkArgs>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkArgs {
    /// As saved; off makes no proxy, and turning it on then needs a restart.
    pub config: NetworkConfig,
    /// This device's private proxy folder (its CA).
    pub directory: PathBuf,
    /// The bundled guest helpers (`ittrust`, `itproxy`) the worker installs.
    pub guest_tools: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsbArgs {
    /// The QEMU-only usbmuxd.
    pub daemon: PathBuf,
    /// This device record's pairing folder.
    pub pairing: PathBuf,
    pub log: PathBuf,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// One touch batch; the worker stamps it on arrival.
    Touch {
        contacts: Vec<Contact>,
    },
    /// 0 Home, 1 Power, 2 Volume up, 3 Volume down.
    Button {
        button: u32,
        down: bool,
    },
    Pause {
        paused: bool,
    },
    /// Shut iOS down cleanly.
    Stop,
    /// Release every held contact and button (focus loss, resize).
    CancelInput,
    Motion {
        acceleration: Option<[f64; 3]>,
        gyro: Option<[f64; 3]>,
    },
    /// Write the current frame as a BMP.
    Screenshot {
        path: PathBuf,
    },
    /// A typed guest-agent request (see `lighttouch_sys::LT_GUEST_*`).
    Guest {
        operation: u32,
        argument: String,
        timeout_ms: u32,
    },
    /// Start (with a path) or stop (without) an engine profile recording.
    Profile {
        path: Option<PathBuf>,
    },
    /// Mute or unmute host audio output without pausing the guest.
    Mute {
        muted: bool,
    },
    /// Change the route, archive date or LAN access live (the settings were saved already).
    Network {
        config: NetworkConfig,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// Verifying the device's files before boot: bytes hashed so far and in total.
    Admission {
        done: u64,
        total: u64,
    },
    Board {
        info: BoardInfo,
    },
    /// The engine started.
    Started,
    /// The guest USB link: the usbmux socket app services connect to, or why there is none.
    Usb {
        socket: Option<PathBuf>,
        error: Option<String>,
    },
    Status(StatusEvent),
    /// A new frame is in shared memory.
    Frame {
        sequence: u64,
        /// When the engine published it (`clock::monotonic_ns`), for the frame inspector's timeline.
        #[serde(default)]
        published_ns: u64,
    },
    /// A command failed; the session continues.
    Error {
        command: String,
        message: String,
    },
    /// The newest touch batch was rejected and every contact released.
    InputRejected {
        message: String,
    },
    GuestResult {
        request_id: u64,
        state: u32,
        status: i32,
        output: String,
    },
    Screenshot {
        path: PathBuf,
    },
    /// The session's networking, sent when it changes.
    Network(NetworkStatus),
    /// The worker is about to exit. `clean` means iOS confirmed shutdown and the state is clean.
    Exit {
        clean: bool,
        dirty: bool,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct StatusEvent {
    pub state: State,
    pub dirty: bool,
    pub frames: u64,
    pub engine_ready: bool,
    pub display_sleeping: bool,
    pub shutdown_phase: u32,
    pub guest_shutdown_confirmed: bool,
    pub storage_failed: bool,
    /// Process CPU time of the worker, for the performance overlay.
    pub cpu_ns: u64,
    /// The worker's resident memory, for the performance overlay.
    #[serde(default)]
    pub resident_bytes: u64,
}

/// What this session's networking is doing, apart from the saved settings: the route it applied, the guest's setup,
/// and the proxy's activity (like Android's Current session).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkStatus {
    /// The engine forwards to a proxy socket, so every network setting applies live.
    pub proxy: bool,
    pub applied: NetworkConfig,
    /// "Waiting for guest tools…", "Ready", or what went wrong.
    pub setup: String,
    pub setup_error: bool,
    pub activity: String,
    pub connections: u64,
    pub completed: u64,
    pub failed: u64,
    pub last_result: String,
    pub last_result_error: bool,
    /// About a requested change: waiting for the engine, needs a restart, or failed.
    pub notice: String,
}

impl Command {
    pub fn to_line(&self) -> String {
        let mut line = serde_json::to_string(self).expect("commands serialize");
        line.push('\n');
        line
    }
}

impl Event {
    pub fn to_line(&self) -> String {
        let mut line = serde_json::to_string(self).expect("events serialize");
        line.push('\n');
        line
    }

    pub fn parse(line: &str) -> Option<Self> {
        serde_json::from_str(line).ok()
    }
}
