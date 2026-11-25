//! The fake cellular network of a device with a modem (iPhone 3GS, iPhone 4): its settings, saved with the record and
//! applied by the core at every boot and live (`lt_library_*carrier`, `lt_runtime_reload_carrier`), the modem's
//! network-side events (calls, texts) and its polled state. The rules are the modem's, checked in the core.
use crate::ffi::{Result, chars, check, cstring};
use lighttouch_sys as sys;
use serde::{Deserialize, Serialize};
use std::ffi::c_char;

/// A device's carrier settings (`lt_carrier`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CarrierSettings {
    pub carrier: String,
    /// MCC (3 digits) + MNC (2 or 3).
    pub mcc_mnc: String,
    pub registered: bool,
    pub sim_present: bool,
    /// Signal bars, 0-5.
    pub bars: u32,
    /// Where a modem with a GPS receiver (the 3GS's) says the phone is, in degrees.
    pub latitude: f64,
    pub longitude: f64,
    /// What Call… and Send text… last used.
    pub last_caller: String,
    pub last_sender: String,
}

impl Default for CarrierSettings {
    fn default() -> Self {
        let mut raw = sys::lt_carrier::new();
        unsafe { sys::lt_carrier_defaults(&mut raw) };
        Self::from_raw(&raw)
    }
}

fn put(out: &mut [c_char], value: &str) -> Result<()> {
    let bytes = cstring(value)?;
    let bytes = bytes.as_bytes_with_nul();
    if bytes.len() > out.len() {
        return Err(crate::Error::invalid("too long"));
    }
    for (slot, &b) in out.iter_mut().zip(bytes) {
        *slot = b as c_char;
    }
    Ok(())
}

impl CarrierSettings {
    pub(crate) fn from_raw(raw: &sys::lt_carrier) -> Self {
        Self {
            carrier: chars(&raw.carrier),
            mcc_mnc: chars(&raw.mcc_mnc),
            registered: raw.registered != 0,
            sim_present: raw.sim_present != 0,
            bars: raw.bars,
            latitude: raw.latitude,
            longitude: raw.longitude,
            last_caller: chars(&raw.last_caller),
            last_sender: chars(&raw.last_sender),
        }
    }

    pub(crate) fn to_raw(&self) -> Result<sys::lt_carrier> {
        let mut raw = sys::lt_carrier::new();
        let long = |what: &str| crate::Error::invalid(format!("{what} is too long"));
        put(&mut raw.carrier, &self.carrier).map_err(|_| long("The carrier name"))?;
        put(&mut raw.mcc_mnc, &self.mcc_mnc).map_err(|_| long("The MCC and MNC"))?;
        raw.registered = self.registered as u32;
        raw.sim_present = self.sim_present as u32;
        raw.bars = self.bars;
        raw.latitude = self.latitude;
        raw.longitude = self.longitude;
        put(&mut raw.last_caller, &self.last_caller).map_err(|_| long("The caller"))?;
        put(&mut raw.last_sender, &self.last_sender).map_err(|_| long("The sender"))?;
        Ok(raw)
    }

    /// Why the modem would refuse these settings, as a sentence for the user.
    pub fn check(&self) -> Result<()> {
        let raw = self.to_raw()?;
        let mut error = sys::lt_error::new();
        check(unsafe { sys::lt_carrier_check(&raw, &mut error) }, &error)
    }

    /// The MCC (first three digits) and MNC.
    pub fn mcc(&self) -> &str {
        self.mcc_mnc.get(..3).unwrap_or(&self.mcc_mnc)
    }
    pub fn mnc(&self) -> &str {
        self.mcc_mnc.get(3..).unwrap_or("")
    }
}

/// Whether a device of this board (`Record::board`) has a modem: only those take carrier settings.
pub fn board_has_modem(board: &str) -> bool {
    cstring(board).is_ok_and(|b| unsafe { sys::lt_board_has_modem(b.as_ptr()) } != 0)
}

/// A caller or SMS sender: 1-20 digits, optionally after a '+'.
pub fn number_ok(number: &str) -> bool {
    cstring(number).is_ok_and(|n| unsafe { sys::lt_carrier_number_ok(n.as_ptr()) } != 0)
}

/// How much of one SMS `text` takes and of how much: (septets, 160) in the GSM alphabet, else (UTF-16 units, 70);
/// the limit is 0 for text the modem can't send.
pub fn sms_length(text: &str) -> (u32, u32) {
    let Ok(text) = cstring(text) else { return (0, 0) };
    let mut limit = 0;
    let used = unsafe { sys::lt_carrier_sms_length(text.as_ptr(), &mut limit) };
    (used, limit)
}

pub fn sms_text_ok(text: &str) -> bool {
    let (used, limit) = sms_length(text);
    !text.is_empty() && used <= limit
}

/// The modem's state as the core reports it (`lt_runtime_modem_status`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "kebab-case")]
pub struct ModemStatus {
    pub carrier: String,
    pub mcc_mnc: String,
    pub registered: bool,
    pub sim_present: bool,
    pub signal_dbm: i32,
    /// idle, dialing, alerting, incoming, active or held.
    pub call_state: String,
    pub emergency_call: bool,
    pub last_dialed: String,
    /// The last text the phone sent: "<number>|<text>".
    pub last_mo_sms: String,
    pub mo_sms_count: u64,
    /// The modem has a GPS receiver, and the position it reports ("" for none).
    pub gps: bool,
    pub gps_fix: String,
    /// The link to the phone is up and registered.
    pub attached: bool,
    /// The modem's refusal of the last write this status reflects.
    pub error: Option<String>,
    /// A write since is not reflected yet.
    pub pending: bool,
}

impl ModemStatus {
    pub fn parse(json: &str) -> Option<Self> {
        serde_json::from_str(json).ok()
    }

    /// The other end of the call the phone is placing can pick up.
    pub fn can_answer(&self) -> bool {
        matches!(self.call_state.as_str(), "dialing" | "alerting")
    }

    /// Some call is under way, ringing or connected.
    pub fn in_call(&self) -> bool {
        !self.call_state.is_empty() && self.call_state != "idle"
    }

    /// The last text the phone sent, as (number, text).
    pub fn last_sent(&self) -> Option<(&str, &str)> {
        // "|" (nothing sent yet) has no number.
        self.last_mo_sms.split_once('|').filter(|(number, _)| !number.is_empty())
    }

    /// The call state for people: "Ringing", "Connected"…
    pub fn call_label(&self) -> &'static str {
        match self.call_state.as_str() {
            "dialing" => "Calling out",
            "alerting" => "Calling out (ringing)",
            "incoming" => "Ringing",
            "active" => "Connected",
            "held" => "On hold",
            _ => "No call",
        }
    }
}

/// The modem's refusal in words for people: the engine's own sentences name AT-level conditions.
pub fn refusal_text(error: &str) -> String {
    if error.starts_with("the modem is not in a state") {
        "The device isn't on the network yet: iOS may still be starting, or Registered or SIM card is off".into()
    } else {
        error.to_string()
    }
}

/// A network-side event for the running modem (`lt_runtime_modem`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum ModemAction {
    /// Ring the phone from this number.
    IncomingCall { number: String },
    /// The other end picks up the call the phone is making.
    RemoteAnswer,
    /// The other end ends the call, ringing or connected.
    RemoteHangup,
    /// A text arrives from this number.
    IncomingSms { number: String, text: String },
}

impl ModemAction {
    pub(crate) fn wire(&self) -> (&'static str, String) {
        match self {
            Self::IncomingCall { number } => ("incoming-call", number.clone()),
            Self::RemoteAnswer => ("remote-answer", "1".into()),
            Self::RemoteHangup => ("remote-hangup", "1".into()),
            Self::IncomingSms { number, text } => ("incoming-sms", format!("{number}|{text}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_rules_come_from_the_core() {
        let d = CarrierSettings::default();
        assert_eq!((d.carrier.as_str(), d.mcc(), d.mnc(), d.bars), ("Light Touch", "001", "01", 5));
        assert!(d.check().is_ok());
        let mut bad = d.clone();
        bad.carrier = "say \"hi\"".into();
        assert!(bad.check().is_err());
        bad.carrier = "x".repeat(33);
        assert!(bad.check().is_err());
        assert!(board_has_modem("n90ap") && !board_has_modem("n72ap"));
        assert!(number_ok("+15555550100") && !number_ok("555 0100"));
        assert_eq!(sms_length("Hi ["), (5, 160));
        assert!(!sms_text_ok("") && sms_text_ok("Hello") && !sms_text_ok(&"a".repeat(161)));
    }

    #[test]
    fn modem_status_parses_the_engine_json() {
        let s = ModemStatus::parse(
            r#"{"carrier":"Light Touch","mcc-mnc":"00101","call-state":"alerting","last-dialed":"5551234",
                "last-mo-sms":"5551234|hi | there","gps-fix":"","sim-lock":"READY","registered":true,
                "sim-present":true,"emergency-call":false,"attached":true,"gps":false,"signal-dbm":-63,
                "mo-sms-count":2,"power-offs":1,"error":"nope"}"#,
        )
        .unwrap();
        assert!(s.can_answer() && s.in_call() && s.attached);
        assert_eq!(s.last_sent(), Some(("5551234", "hi | there")));
        assert_eq!(s.error.as_deref(), Some("nope"));
        assert_eq!(ModemStatus { last_mo_sms: "|".into(), ..Default::default() }.last_sent(), None);
    }
}
