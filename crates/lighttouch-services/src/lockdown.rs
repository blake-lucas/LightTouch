//! lockdownd: the device's service directory on port 62078. Plists framed by a big-endian length; after
//! `StartSession` the connection continues inside TLS made with the host's pairing certificate. Old iOS only speaks
//! TLS 1.0 with RSA key exchange and 1024-bit keys, which OpenSSL allows at security level 0.
use crate::usbmux::{Mux, MuxDevice, PairRecord};
use crate::{Error, Io, Result, dictionary, pairing, read_exact_vec, string, to_xml};
use openssl::pkey::PKey;
use openssl::ssl::{SslConnector, SslMethod, SslStream, SslVerifyMode, SslVersion};
use openssl::x509::X509;
use plist::{Dictionary, Value};
use std::io::{Read, Write};
use std::time::Duration;

const PORT: u16 = 62078;
const LABEL: &str = "LightTouch";
/// Replies larger than this are refused (an installed-apps list is well under it).
const REPLY_LIMIT: usize = 32 << 20;

/// A connection to a device service: plain, or inside TLS.
pub enum Conn {
    Plain(Box<dyn Io>),
    Tls(Box<SslStream<Box<dyn Io>>>),
}

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Conn::Plain(io) => io.read(buf),
            Conn::Tls(io) => io.read(buf),
        }
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Conn::Plain(io) => io.write(buf),
            Conn::Tls(io) => io.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Conn::Plain(io) => io.flush(),
            Conn::Tls(io) => io.flush(),
        }
    }
}

impl Conn {
    /// Send one length-framed XML plist.
    pub fn send(&mut self, message: Dictionary) -> Result<()> {
        let body = to_xml(&Value::Dictionary(message))?;
        let mut frame = Vec::with_capacity(4 + body.len());
        frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frame.extend_from_slice(&body);
        self.write_all(&frame)?;
        self.flush()?;
        Ok(())
    }

    /// Receive one length-framed plist dictionary.
    pub fn receive(&mut self) -> Result<Dictionary> {
        let head = read_exact_vec(self, 4)?;
        let length = u32::from_be_bytes(head.try_into().unwrap()) as usize;
        if length == 0 || length > REPLY_LIMIT {
            return Err(Error("the guest sent an invalid reply".into()));
        }
        dictionary(&read_exact_vec(self, length)?)
    }

    fn into_tls(self, record: &PairRecord, ios_major: u32) -> Result<Conn> {
        match self {
            Conn::Plain(io) => Ok(Conn::Tls(Box::new(tls(io, record, ios_major)?))),
            tls => Ok(tls),
        }
    }
}

/// Wrap `io` in TLS as the client, presenting the host certificate. The device's certificate is ours (we signed
/// it when pairing), so it isn't verified.
fn tls(io: Box<dyn Io>, record: &PairRecord, ios_major: u32) -> Result<SslStream<Box<dyn Io>>> {
    let mut builder = SslConnector::builder(SslMethod::tls_client())?;
    builder.set_verify(SslVerifyMode::NONE);
    builder.set_security_level(0);
    builder.set_min_proto_version(Some(SslVersion::TLS1))?;
    // Before iOS 10, lockdownd's TLS fails on newer versions and ECDHE key exchange (iOS 5.1.1 closes the
    // connection); TLS 1.0 with RSA key exchange works on every version LightTouch runs.
    if ios_major < 10 {
        builder.set_max_proto_version(Some(SslVersion::TLS1))?;
        builder.set_cipher_list("ALL:!aNULL:!eNULL:!kECDHE:@SECLEVEL=0")?;
    } else {
        builder.set_cipher_list("ALL:!aNULL:!eNULL:@SECLEVEL=0")?;
    }
    // Old iOS predates the renegotiation extension (RFC 5746); OpenSSL 3 refuses such servers unless allowed
    // (SSL_OP_LEGACY_SERVER_CONNECT, which the crate doesn't name).
    builder.set_options(openssl::ssl::SslOptions::from_bits_retain(0x4));
    let certificate = X509::from_pem(&record.host_certificate)?;
    let key = PKey::private_key_from_pem(&record.host_private_key)?;
    builder.set_certificate(&certificate)?;
    builder.set_private_key(&key)?;
    let mut config = builder.build().configure()?;
    config.set_verify_hostname(false);
    config.set_use_server_name_indication(false);
    config.connect("guest", io).map_err(|e| {
        let reason = match e {
            openssl::ssl::HandshakeError::SetupFailure(stack) => stack.to_string(),
            openssl::ssl::HandshakeError::Failure(mid) | openssl::ssl::HandshakeError::WouldBlock(mid) => {
                mid.error().to_string()
            }
        };
        Error(format!("The secure session with the guest failed ({reason})."))
    })
}

/// "3.1.3" → (3, 1, 3).
pub fn parse_version(text: &str) -> (u32, u32, u32) {
    let mut parts = text.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    (parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0))
}

pub struct Lockdown {
    conn: Conn,
    mux: Mux,
    device: MuxDevice,
    record: Option<PairRecord>,
    /// The guest's iOS version.
    pub version: (u32, u32, u32),
}

impl Lockdown {
    /// Connect and check that lockdownd answers. No session yet: only public values can be read.
    pub fn connect(mux: &Mux, device: &MuxDevice, timeout: Duration) -> Result<Self> {
        let conn = Conn::Plain(mux.connect(device, PORT, timeout)?);
        let mut lockdown = Self { conn, mux: mux.clone(), device: device.clone(), record: None, version: (0, 0, 0) };
        let reply = lockdown.request("QueryType", Dictionary::new())?;
        if string(&reply, "Type").as_deref() != Some("com.apple.mobile.lockdown") {
            return Err(Error("the guest's lockdown service didn't identify itself".into()));
        }
        if let Some(Value::String(version)) = lockdown.value(None, Some("ProductVersion")).ok() {
            lockdown.version = parse_version(&version);
        }
        Ok(lockdown)
    }

    /// Connect, then start the TLS session with the stored pairing. usbmuxd pairs when the guest attaches, so
    /// a missing record means the guest is still starting.
    pub fn session(mux: &Mux, device: &MuxDevice, timeout: Duration) -> Result<Self> {
        let mut lockdown = Self::connect(mux, device, timeout)?;
        let record = match mux.pair_record(&device.udid)? {
            Some(record) => record,
            // The Windows daemon is built without libimobiledevice, so it never pairs: pair here, as
            // libimobiledevice's clients do. LIGHTTOUCH_SELF_PAIR does the same elsewhere, for testing.
            None if cfg!(windows) || std::env::var_os("LIGHTTOUCH_SELF_PAIR").is_some() => lockdown.pair()?,
            None => {
                return Err(Error("The guest isn't paired yet; try again once iOS has finished starting.".into()));
            }
        };
        let buid = if record.system_buid.is_empty() { mux.buid()? } else { record.system_buid.clone() };
        if lockdown.version.0 < 7 {
            let pair = Dictionary::from_iter([
                ("DeviceCertificate", Value::Data(record.device_certificate.clone())),
                ("HostCertificate", Value::Data(record.host_certificate.clone())),
                ("HostID", Value::from(record.host_id.clone())),
                ("RootCertificate", Value::Data(record.root_certificate.clone())),
                ("SystemBUID", Value::from(buid.clone())),
            ]);
            lockdown.request(
                "ValidatePair",
                Dictionary::from_iter([("PairRecord", Value::Dictionary(pair)), ("ProtocolVersion", Value::from("2"))]),
            )?;
        }
        let reply = lockdown.request(
            "StartSession",
            Dictionary::from_iter([("HostID", Value::from(record.host_id.clone())), ("SystemBUID", Value::from(buid))]),
        )?;
        if reply.get("EnableSessionSSL").and_then(|v| v.as_boolean()).unwrap_or(false) {
            let conn = std::mem::replace(&mut lockdown.conn, Conn::Plain(Box::new(Empty)));
            lockdown.conn = conn.into_tls(&record, lockdown.version.0)?;
        }
        lockdown.record = Some(record);
        Ok(lockdown)
    }

    /// Pair with the guest and store the record with the daemon, as libimobiledevice's `lockdownd_pair` does: new
    /// root and host keys, a device certificate for the guest's public key, all signed by the root. iOS 7 and
    /// later ask on the guest's screen first.
    fn pair(&mut self) -> Result<PairRecord> {
        let public_key = match self.value(None, Some("DevicePublicKey"))? {
            Value::Data(key) => key,
            _ => return Err(Error("the guest has no public key to pair with".into())),
        };
        // Read before pairing: iOS 7 drops the connection when it's asked afterwards.
        let wifi = self.value(None, Some("WiFiAddress")).ok();
        let buid = self.mux.buid()?;
        // While iOS 7 and later wait for Trust to be tapped, every retry asks with the same identity.
        let (host_id, keys) = pairing::pending(&self.device.udid, &public_key, self.version)?;
        let mut record = Dictionary::from_iter([
            ("DeviceCertificate", Value::Data(keys.device_certificate)),
            ("HostCertificate", Value::Data(keys.host_certificate)),
            ("HostID", Value::from(host_id)),
            ("RootCertificate", Value::Data(keys.root_certificate)),
            ("SystemBUID", Value::from(buid)),
        ]);
        let options = Dictionary::from_iter([("ExtendedPairingErrors", Value::Boolean(true))]);
        let reply = self
            .request(
                "Pair",
                Dictionary::from_iter([
                    ("PairRecord", Value::Dictionary(record.clone())),
                    ("ProtocolVersion", Value::from("2")),
                    ("PairingOptions", Value::Dictionary(options)),
                ]),
            )
            .map_err(|e| match e.0.contains("PairingDialogResponsePending") {
                true => Error("Tap Trust on the guest's screen to let LightTouch install apps.".into()),
                false => e,
            })?;
        record.insert("HostPrivateKey".into(), Value::Data(keys.host_private_key));
        record.insert("RootPrivateKey".into(), Value::Data(keys.root_private_key));
        if let Some(bag @ Value::Data(_)) = reply.get("EscrowBag") {
            record.insert("EscrowBag".into(), bag.clone());
        }
        if let Some(wifi @ Value::String(_)) = wifi {
            record.insert("WiFiMACAddress".into(), wifi);
        }
        let mut bytes = Vec::new();
        Value::Dictionary(record).to_writer_binary(&mut bytes)?;
        self.mux.save_pair_record(&self.device, &bytes)?;
        pairing::forget(&self.device.udid);
        PairRecord::parse(&bytes)
    }

    fn request(&mut self, request: &str, mut fields: Dictionary) -> Result<Dictionary> {
        fields.insert("Label".into(), LABEL.into());
        fields.insert("Request".into(), request.into());
        self.conn.send(fields)?;
        let reply = self.conn.receive()?;
        if let Some(error) = string(&reply, "Error") {
            return Err(Error(format!("{request} failed: {error}")));
        }
        Ok(reply)
    }

    /// A lockdown value (`GetValue`): one key, or a whole domain when `key` is `None`.
    pub fn value(&mut self, domain: Option<&str>, key: Option<&str>) -> Result<Value> {
        let mut fields = Dictionary::new();
        if let Some(domain) = domain {
            fields.insert("Domain".into(), domain.into());
        }
        if let Some(key) = key {
            fields.insert("Key".into(), key.into());
        }
        let reply = self.request("GetValue", fields)?;
        reply
            .get("Value")
            .cloned()
            .ok_or_else(|| Error(format!("the guest has no value for {}", key.unwrap_or("that"))))
    }

    pub fn string_value(&mut self, key: &str) -> Result<String> {
        match self.value(None, Some(key))? {
            Value::String(s) => Ok(s),
            other => Err(Error(format!("{key} isn't text ({other:?})"))),
        }
    }

    /// Start a service and connect to it (inside TLS if it asks for that).
    pub fn service(&mut self, name: &str, timeout: Duration) -> Result<Conn> {
        let reply = self.request("StartService", Dictionary::from_iter([("Service", Value::from(name))]))?;
        let port = reply
            .get("Port")
            .and_then(|v| v.as_unsigned_integer())
            .filter(|p| *p > 0 && *p <= u16::MAX as u64)
            .ok_or_else(|| Error(format!("the guest didn't start {name}")))? as u16;
        let conn = Conn::Plain(self.mux.connect(&self.device, port, timeout)?);
        if reply.get("EnableServiceSSL").and_then(|v| v.as_boolean()).unwrap_or(false) {
            let record = self.record.as_ref().ok_or("a secure service needs a session")?;
            return conn.into_tls(record, self.version.0);
        }
        Ok(conn)
    }
}

/// A placeholder stream while a connection is being moved into TLS.
struct Empty;

impl Read for Empty {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Ok(0)
    }
}

impl Write for Empty {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
