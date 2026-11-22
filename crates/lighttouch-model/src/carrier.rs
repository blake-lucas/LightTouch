//! Carrier: the fake cellular network of a device with a modem (iPhone 3GS, iPhone 4), like Light Touch for Mac's
//! Carrier panel. Its settings are the device's, saved with its record by the core, which applies them at every boot;
//! a change while it runs is written to the modem too. Calls and texts go straight to the running modem, and its
//! state comes back from the session (`Session::modem`).
use crate::{Device, Model};
use lighttouch_core::carrier::{self, CarrierSettings, ModemAction, ModemStatus};

/// The settings of the device last asked about, as saved.
pub(crate) type Cache = Option<(String, CarrierSettings)>;

impl Model {
    /// The current device (the running one, else the selected one) when it has a modem.
    pub fn carrier_device(&self) -> Option<&Device> {
        self.current().filter(|d| carrier::board_has_modem(&d.board))
    }

    /// The carrier settings of `carrier_device`, as saved (the defaults until changed).
    pub fn carrier_settings(&mut self) -> Option<CarrierSettings> {
        let id = self.carrier_device()?.id.clone();
        if let Some((cached, settings)) = &self.carrier {
            if *cached == id {
                return Some(settings.clone());
            }
        }
        let settings = self.library().ok()?.carrier(&id).ok()?;
        self.carrier = Some((id, settings.clone()));
        Some(settings)
    }

    /// Whether `carrier_device` runs now, so calls and texts can reach it.
    pub fn carrier_live(&self) -> bool {
        let Some(device) = self.carrier_device() else { return false };
        self.session.as_ref().is_some_and(|s| s.record_id == device.id && s.phase.active())
    }

    /// The running modem's state, once it has reported.
    pub fn modem_status(&self) -> Option<&ModemStatus> {
        if !self.carrier_live() {
            return None;
        }
        self.session.as_ref()?.modem.as_ref()
    }

    /// What the modem or the core last refused, for the carrier controls.
    pub fn modem_error(&self) -> Option<&str> {
        if !self.carrier_live() {
            return None;
        }
        self.session.as_ref()?.modem_error.as_deref()
    }

    /// Check and save `carrier_device`'s settings; a running device takes what changed at once.
    pub fn set_carrier(&mut self, settings: CarrierSettings) -> Result<(), String> {
        let id = self.carrier_device().map(|d| d.id.clone()).ok_or("This device has no cellular modem")?;
        settings.check().map_err(|e| e.to_string())?;
        self.library()?.set_carrier(&id, &settings).map_err(|e| format!("Couldn't save the carrier settings: {e}"))?;
        let changed_network =
            self.carrier.as_ref().is_none_or(|(cached, old)| *cached != id || !same_modem(old, &settings));
        self.carrier = Some((id.clone(), settings));
        if changed_network {
            if let Some(session) = self.session.as_mut().filter(|s| s.record_id == id) {
                session.reload_carrier();
            }
        }
        Ok(())
    }

    /// Ring the running device from `number` (remembered for next time).
    pub fn ring(&mut self, number: &str) -> Result<(), String> {
        let number = number.trim().to_string();
        if !carrier::number_ok(&number) {
            return Err("A caller is 1 to 20 digits, optionally after a +".into());
        }
        self.remember(|s| s.last_caller = number.clone());
        self.modem(ModemAction::IncomingCall { number })
    }

    /// A text from `number` (remembered for next time) arrives on the running device.
    pub fn text(&mut self, number: &str, text: &str) -> Result<(), String> {
        let number = number.trim().to_string();
        if !carrier::number_ok(&number) {
            return Err("A sender is 1 to 20 digits, optionally after a +".into());
        }
        let (used, limit) = carrier::sms_length(text);
        if text.is_empty() || used > limit {
            return Err(if limit == 0 {
                "Emoji and other characters beyond the SMS alphabet's reach can't be sent".into()
            } else {
                format!("A text is at most {limit} characters")
            });
        }
        self.remember(|s| s.last_sender = number.clone());
        self.modem(ModemAction::IncomingSms { number, text: text.to_string() })
    }

    /// The other end picks up the call the phone is making.
    pub fn remote_answer(&mut self) -> Result<(), String> {
        self.modem(ModemAction::RemoteAnswer)
    }

    /// The other end ends the call, ringing or connected.
    pub fn remote_hangup(&mut self) -> Result<(), String> {
        self.modem(ModemAction::RemoteHangup)
    }

    fn modem(&mut self, action: ModemAction) -> Result<(), String> {
        if !self.carrier_live() {
            return Err("Start the device first".into());
        }
        let session = self.session.as_mut().ok_or("Start the device first")?;
        session.modem(action);
        Ok(())
    }

    /// Save a change to the numbers the controls remember (not a network change).
    fn remember(&mut self, change: impl FnOnce(&mut CarrierSettings)) {
        if let Some(mut settings) = self.carrier_settings() {
            change(&mut settings);
            let _ = self.set_carrier(settings);
        }
    }
}

/// The settings the modem sees are the same (only the app's own fields differ).
fn same_modem(a: &CarrierSettings, b: &CarrierSettings) -> bool {
    a.carrier == b.carrier
        && a.mcc_mnc == b.mcc_mnc
        && a.registered == b.registered
        && a.sim_present == b.sim_present
        && a.bars == b.bars
        && a.latitude == b.latitude
        && a.longitude == b.longitude
}

/// What the modem reported, kept by the session: its state and its last refusal.
pub fn take_status(
    modem: &mut Option<ModemStatus>,
    error: &mut Option<String>,
    sent: &mut Vec<(String, String)>,
    status: ModemStatus,
) {
    // A text the device sent since the last report (the count goes up; the first report only sets the baseline).
    if let (Some(old), Some((number, text))) = (modem.as_ref(), status.last_sent()) {
        if status.mo_sms_count > old.mo_sms_count {
            sent.push((number.to_string(), text.to_string()));
            if sent.len() > 20 {
                sent.remove(0);
            }
        }
    }
    if !status.pending {
        if let Some(refusal) = status.error.clone() {
            if modem.as_ref().is_none_or(|old| old.error.as_ref() != Some(&refusal) || old.pending) {
                *error = Some(carrier::refusal_text(&refusal));
            }
        }
    }
    *modem = Some(status);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(error: Option<&str>, pending: bool, sent: (&str, u64)) -> ModemStatus {
        ModemStatus {
            error: error.map(str::to_string),
            pending,
            last_mo_sms: sent.0.into(),
            mo_sms_count: sent.1,
            ..Default::default()
        }
    }

    #[test]
    fn refusals_show_once_and_sent_texts_are_kept() {
        let (mut modem, mut error, mut sent) = (None, None, Vec::new());
        take_status(&mut modem, &mut error, &mut sent, status(None, false, ("|", 0)));
        // A write's snapshot that doesn't reflect it yet carries no verdict.
        take_status(&mut modem, &mut error, &mut sent, status(None, true, ("|", 0)));
        assert_eq!(error, None);
        let refusal = "the modem is not in a state to ring (registered, radio on, channel open?)";
        take_status(&mut modem, &mut error, &mut sent, status(Some(refusal), false, ("|", 0)));
        assert!(error.as_deref().is_some_and(|e| e.starts_with("The device isn't on the network yet")));
        // The same refusal reported again (the engine keeps it until the next write) doesn't come back once cleared.
        error = None;
        take_status(&mut modem, &mut error, &mut sent, status(Some(refusal), false, ("|", 0)));
        assert_eq!(error, None);
        take_status(&mut modem, &mut error, &mut sent, status(None, false, ("5551234|hi", 1)));
        assert_eq!(sent, vec![("5551234".to_string(), "hi".to_string())]);
    }
}
