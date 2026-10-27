//! The installation proxy (`com.apple.mobile.installation_proxy`): lists installed apps and installs a package
//! staged in `PublicStaging`. Requests and replies are lockdown-framed plists.
use crate::lockdown::Conn;
use crate::{Error, Result, string};
use plist::{Dictionary, Value};

/// One installed user app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledApp {
    pub bundle_id: String,
    pub name: Option<String>,
    pub version: Option<String>,
    pub short_version: Option<String>,
}

/// A status reply while installing.
#[derive(Debug, Clone, PartialEq)]
pub enum InstallStatus {
    Progress { status: String, percent: Option<u64> },
    Complete,
}

fn failure(reply: &Dictionary) -> Option<Error> {
    let error = string(reply, "Error")?;
    Some(Error(string(reply, "ErrorDescription").filter(|d| !d.is_empty()).unwrap_or(error)))
}

pub fn browse_user_apps(conn: &mut Conn) -> Result<Vec<InstalledApp>> {
    conn.send(Dictionary::from_iter([
        ("Command", Value::from("Browse")),
        ("ClientOptions", Value::Dictionary(Dictionary::from_iter([("ApplicationType", Value::from("User"))]))),
    ]))?;
    let mut apps = Vec::new();
    loop {
        let reply = conn.receive()?;
        if let Some(error) = failure(&reply) {
            return Err(error);
        }
        for app in reply.get("CurrentList").and_then(|v| v.as_array()).into_iter().flatten() {
            let Some(app) = app.as_dictionary() else { continue };
            let Some(bundle_id) = string(app, "CFBundleIdentifier") else { continue };
            apps.push(InstalledApp {
                bundle_id,
                name: string(app, "CFBundleDisplayName").or_else(|| string(app, "CFBundleName")),
                version: string(app, "CFBundleVersion"),
                short_version: string(app, "CFBundleShortVersionString"),
            });
        }
        match string(&reply, "Status").as_deref() {
            Some("Complete") | None => return Ok(apps),
            _ => {}
        }
    }
}

/// Ask for `package` (a path under the AFC root) to be installed. Follow with `install_status` until complete.
pub fn submit_install(conn: &mut Conn, package: &str) -> Result<()> {
    conn.send(Dictionary::from_iter([
        ("Command", Value::from("Install")),
        ("PackagePath", Value::from(package)),
        ("ClientOptions", Value::Dictionary(Dictionary::new())),
    ]))
}

pub fn install_status(conn: &mut Conn) -> Result<InstallStatus> {
    let reply = conn.receive()?;
    if let Some(error) = failure(&reply) {
        return Err(error);
    }
    Ok(match string(&reply, "Status").as_deref() {
        Some("Complete") => InstallStatus::Complete,
        status => InstallStatus::Progress {
            status: status.unwrap_or("Installing").to_string(),
            percent: reply.get("PercentComplete").and_then(|v| v.as_unsigned_integer()),
        },
    })
}
