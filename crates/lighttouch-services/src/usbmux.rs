//! usbmux client: the plist protocol a usbmuxd speaks on its client socket. Each request is its own connection;
//! `connect` turns the connection into a byte stream to a TCP port on the device.
use crate::{Error, Io, Result, dictionary, read_exact_vec, string, to_xml};
use plist::{Dictionary, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

const VERSION_PLIST: u32 = 1;
const MESSAGE_PLIST: u32 = 8;

/// A usbmuxd client socket.
#[derive(Debug, Clone)]
pub struct Mux {
    pub socket: PathBuf,
}

/// A device the daemon has attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MuxDevice {
    pub id: u64,
    pub udid: String,
}

/// The host's pairing with one device, as usbmuxd stores it.
#[derive(Debug, Clone)]
pub struct PairRecord {
    pub host_id: String,
    pub system_buid: String,
    pub host_certificate: Vec<u8>,
    pub host_private_key: Vec<u8>,
    pub root_certificate: Vec<u8>,
    pub device_certificate: Vec<u8>,
}

impl PairRecord {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let dict = dictionary(bytes)?;
        let data = |key: &str| -> Result<Vec<u8>> {
            dict.get(key)
                .and_then(|v| v.as_data())
                .map(<[u8]>::to_vec)
                .ok_or_else(|| Error(format!("the pairing record has no {key}")))
        };
        Ok(Self {
            host_id: string(&dict, "HostID").ok_or("the pairing record has no HostID")?,
            system_buid: string(&dict, "SystemBUID").unwrap_or_default(),
            host_certificate: data("HostCertificate")?,
            host_private_key: data("HostPrivateKey")?,
            root_certificate: data("RootCertificate")?,
            device_certificate: data("DeviceCertificate")?,
        })
    }
}

impl Mux {
    pub fn new(socket: impl Into<PathBuf>) -> Self {
        Self { socket: socket.into() }
    }

    fn open(&self) -> Result<Box<dyn Io>> {
        open_socket(&self.socket)
    }

    fn exchange(&self, io: &mut dyn Io, mut request: Dictionary) -> Result<Dictionary> {
        request.insert("ClientVersionString".into(), "LightTouch".into());
        request.insert("ProgName".into(), "LightTouch".into());
        let body = to_xml(&Value::Dictionary(request))?;
        let mut header = Vec::with_capacity(16 + body.len());
        header.extend_from_slice(&(16 + body.len() as u32).to_le_bytes());
        header.extend_from_slice(&VERSION_PLIST.to_le_bytes());
        header.extend_from_slice(&MESSAGE_PLIST.to_le_bytes());
        header.extend_from_slice(&1u32.to_le_bytes());
        header.extend_from_slice(&body);
        io.write_all(&header)?;
        let head = read_exact_vec(io, 16)?;
        let length = u32::from_le_bytes(head[0..4].try_into().unwrap()) as usize;
        if !(16..=16 << 20).contains(&length) {
            return Err(Error("the USB bridge sent an invalid reply".into()));
        }
        dictionary(&read_exact_vec(io, length - 16)?)
    }

    fn request(&self, request: Dictionary) -> Result<Dictionary> {
        let mut io = self.open()?;
        self.exchange(io.as_mut(), request)
    }

    pub fn devices(&self) -> Result<Vec<MuxDevice>> {
        let reply = self.request(Dictionary::from_iter([("MessageType", Value::from("ListDevices"))]))?;
        let list = reply.get("DeviceList").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        Ok(list
            .iter()
            .filter_map(|entry| {
                let props = entry.as_dictionary()?.get("Properties")?.as_dictionary()?;
                Some(MuxDevice {
                    id: props.get("DeviceID")?.as_unsigned_integer()?,
                    udid: string(props, "SerialNumber")?,
                })
            })
            .collect())
    }

    /// The stored pairing with `udid`, or `None` if there isn't one yet.
    pub fn pair_record(&self, udid: &str) -> Result<Option<PairRecord>> {
        let reply = self.request(Dictionary::from_iter([
            ("MessageType", Value::from("ReadPairRecord")),
            ("PairRecordID", Value::from(udid)),
        ]))?;
        match reply.get("PairRecordData").and_then(|v| v.as_data()) {
            Some(data) => PairRecord::parse(data).map(Some),
            None => Ok(None),
        }
    }

    /// Store a pairing (a binary plist) for the daemon to keep in its pairing folder.
    pub fn save_pair_record(&self, device: &MuxDevice, record: &[u8]) -> Result<()> {
        let reply = self.request(Dictionary::from_iter([
            ("MessageType", Value::from("SavePairRecord")),
            ("PairRecordID", Value::from(device.udid.as_str())),
            ("PairRecordData", Value::Data(record.to_vec())),
            ("DeviceID", Value::from(device.id)),
        ]))?;
        match reply.get("Number").and_then(|v| v.as_unsigned_integer()) {
            Some(0) => Ok(()),
            other => Err(Error(format!("the USB bridge couldn't save the pairing ({other:?})"))),
        }
    }

    pub fn buid(&self) -> Result<String> {
        let reply = self.request(Dictionary::from_iter([("MessageType", Value::from("ReadBUID"))]))?;
        string(&reply, "BUID").ok_or_else(|| Error("the USB bridge has no host identifier".into()))
    }

    /// A byte stream to TCP `port` on the device. Reads time out after `timeout`.
    pub fn connect(&self, device: &MuxDevice, port: u16, timeout: Duration) -> Result<Box<dyn Io>> {
        let mut io = open_socket_with_timeout(&self.socket, Some(timeout))?;
        let reply = self.exchange(
            io.as_mut(),
            Dictionary::from_iter([
                ("MessageType", Value::from("Connect")),
                ("DeviceID", Value::from(device.id)),
                // Network byte order, as an integer.
                ("PortNumber", Value::from(port.swap_bytes() as u64)),
            ]),
        )?;
        match reply.get("Number").and_then(|v| v.as_unsigned_integer()) {
            Some(0) => Ok(io),
            Some(2) => Err(Error("the guest is no longer connected".into())),
            Some(3) => Err(Error(format!("the guest refused a connection to port {port}"))),
            other => Err(Error(format!("the USB bridge couldn't connect to port {port} ({other:?})"))),
        }
    }
}

fn open_socket(path: &Path) -> Result<Box<dyn Io>> {
    open_socket_with_timeout(path, Some(Duration::from_secs(10)))
}

/// The daemon's client address: a Unix socket path, or `host:port` (Windows, where it listens on loopback TCP).
fn open_socket_with_timeout(address: &Path, timeout: Option<Duration>) -> Result<Box<dyn Io>> {
    if let Some(tcp) = address.to_str().and_then(|a| a.parse::<std::net::SocketAddr>().ok()) {
        let stream = std::net::TcpStream::connect_timeout(&tcp, Duration::from_secs(5))
            .map_err(|e| Error(format!("The guest USB bridge isn't reachable ({e}).")))?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(timeout)?;
        stream.set_write_timeout(timeout)?;
        return Ok(Box::new(stream));
    }
    #[cfg(unix)]
    {
        let stream = std::os::unix::net::UnixStream::connect(address)
            .map_err(|e| Error(format!("The guest USB bridge isn't reachable ({e}).")))?;
        stream.set_read_timeout(timeout)?;
        stream.set_write_timeout(timeout)?;
        Ok(Box::new(stream))
    }
    #[cfg(not(unix))]
    Err(Error(format!("Not a guest USB bridge address: {}", address.display())))
}
