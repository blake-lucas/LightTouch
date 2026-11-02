//! Why an iOS app quit, from the places that know: the guest's crash report names the frame that faulted, its system
//! log says what led to it (dyld refusing a symbol, a missing service, the watchdog), and the emulator's log and the
//! guest's serial console say whether the emulation complained first. Together they tell an app that needs a newer
//! iOS from one the emulation broke, which the report's header alone cannot. The Android app's GuestCrashReport and
//! GuestCrashDiagnosis say the same.
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Frames of the faulting thread kept; a report can list hundreds, and the first ones name the crash.
const FRAMES: usize = 24;
const TRACE_FRAMES: usize = 12;
const LOG_TAIL: usize = 60;

/// Where the running device's own logs are.
#[derive(Debug, Clone, Default)]
pub struct DeviceLogs {
    /// The session log, which holds the emulator's output.
    pub engine: Option<std::path::PathBuf>,
    /// The guest's serial console.
    pub serial: Option<std::path::PathBuf>,
}

/// The answer: a verdict first, then the system log of the launch (when one was recorded), the crash report and the
/// tails of the device's logs. `crash` is the guest's newest report (name, text).
pub fn report(logs: &DeviceLogs, crash: Option<(&str, &str)>, system_log: Option<&str>) -> String {
    let summary = crash.filter(|(_, body)| !body.trim().is_empty()).map(|(name, body)| summarize(name, body));
    let engine = tail(logs.engine.as_deref(), LOG_TAIL);
    let serial = tail(logs.serial.as_deref(), 40);
    let mut out = String::new();
    out.push_str(summary.as_ref().map_or(
        "There is no crash report, so the app didn't crash. Either iOS closed it (for example, to free memory) or it \
         is still running.",
        |s| s.verdict.as_str(),
    ));
    out.push('\n');
    if let Some(hint) = engine_hint(&engine) {
        out.push('\n');
        out.push_str(&hint);
        out.push('\n');
    }
    if let Some(log) = system_log {
        out.push_str("\n——— iOS system log, last launch ———\n");
        out.push_str(if log.trim().is_empty() { "(iOS logged nothing)" } else { log });
        out.push('\n');
    }
    if let Some(summary) = &summary {
        out.push_str("\n——— Crash report ———\n");
        out.push_str(&summary.body);
        out.push('\n');
    }
    for (title, lines) in [("Emulator log", &engine), ("Serial console", &serial)] {
        if !lines.is_empty() {
            out.push_str(&format!("\n——— {title} ———\n"));
            for line in lines {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out.trim_end().to_string()
}

/// The last `count` lines of `file`, without reading the whole thing: a session log grows to megabytes.
fn tail(file: Option<&Path>, count: usize) -> Vec<String> {
    let Some(mut handle) = file.and_then(|f| std::fs::File::open(f).ok()) else { return Vec::new() };
    let length = handle.metadata().map(|m| m.len()).unwrap_or(0);
    let start = length.saturating_sub(64 << 10);
    let mut bytes = Vec::new();
    if handle.seek(SeekFrom::Start(start)).is_err() || handle.read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<String> =
        text.lines().filter(|l| !l.trim().is_empty()).map(|l| l.chars().take(400).collect()).collect();
    lines[lines.len().saturating_sub(count)..].to_vec()
}

/// What the emulator said it dropped. A crash in the app's own code right after one of these is not the app's
/// mistake: a graphics entry point that returns 0 hands the app a null it has no way to check.
fn engine_hint(engine: &[String]) -> Option<String> {
    // An entry point that answered nothing is the stronger evidence of the two, so it wins wherever it is.
    for line in engine.iter().rev() {
        if let Some((name, id)) = unhandled(line) {
            return Some(format!(
                "The emulator's log shows the app called {name} (id {id}), a graphics function LightTouch doesn't \
                 support yet, and got 0 back. If the app crashed right after, that is the likely cause."
            ));
        }
    }
    for line in engine.iter().rev() {
        if let Some(at) = line.find("REFUSED ") {
            let what = line[at + 8..].split_whitespace().next().unwrap_or("");
            // draw-error is the app's own GL error, handed back to it as on hardware: common and rarely fatal.
            if !what.is_empty() && !what.starts_with("draw-error") {
                return Some(format!("The emulator's log shows it refused {what}, which may have caused the crash."));
            }
        }
    }
    None
}

/// `UNHANDLED name (id N)`.
fn unhandled(line: &str) -> Option<(&str, &str)> {
    let rest = &line[line.find("UNHANDLED ")? + 10..];
    let name_end = rest.find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
    let (name, rest) = rest.split_at(name_end);
    let id = rest.strip_prefix(" (id ")?;
    let id = &id[..id.find(')')?];
    (!name.is_empty() && !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())).then_some((name, id))
}

/// A verdict sentence, and the report text it was read from.
struct Summary {
    verdict: String,
    body: String,
}

/// Runtime stubs the app calls into; a crash in one of them is really about the frame that called it.
fn stub(image: &str) -> bool {
    ["libobjc", "libsystem_kernel", "libsystem_c", "libsystem_malloc", "libSystem"].iter().any(|s| image.starts_with(s))
        || image == "dyld"
}

/// One crash report, read for the part that explains the crash: the faulting thread's frames, the image it faulted
/// in and that image's path, dyld's message, and whose side the crash is on.
fn summarize(name: &str, raw: &str) -> Summary {
    let text = raw.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = text.lines().collect();
    let process = field(&lines, "Process").map(|p| p.split(" [").next().unwrap_or("").trim()).unwrap_or("");
    let guest = field(&lines, "OS Version")
        .map(|v| v.trim_start_matches("iOS").trim().split(" (").next().unwrap_or("").trim())
        .unwrap_or("");
    let frames = crashed_frames(&lines);
    let images: Vec<&str> = frames.iter().filter_map(|f| frame_image(f)).collect();
    let image = images.first().copied();
    // The frame that called into the runtime stub is the one that made the mistake.
    let caller = images.iter().copied().find(|i| !stub(i)).filter(|c| Some(*c) != image);
    let image_path = binary_image_path(&lines, image);
    let caller_path = binary_image_path(&lines, caller);
    let dyld = block(&lines, "Dyld Error Message:", 4);
    let codes = [field(&lines, "Exception Codes"), field(&lines, "Exception Subtype")]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    let termination = field(&lines, "Termination Reason").unwrap_or("");
    let verdict = verdict(process, guest, image, image_path, caller, caller_path, &dyld, &codes, termination);

    let mut body = String::new();
    if !name.is_empty() {
        body.push_str(name);
        body.push('\n');
    }
    for key in [
        "Process",
        "Date/Time",
        "OS Version",
        "Exception Type",
        "Exception Codes",
        "Exception Subtype",
        "Crashed Thread",
        "Termination Reason",
    ] {
        if let Some(value) = field(&lines, key).filter(|v| !v.is_empty()) {
            body.push_str(&format!("{key}: {value}\n"));
        }
    }
    let specific = block(&lines, "Application Specific Information:", 6);
    if !specific.is_empty() {
        body.push_str("Application Specific Information:\n");
        for line in &specific {
            body.push_str(line.trim());
            body.push('\n');
        }
    }
    let at = |path: Option<&str>| path.map(|p| format!("  ({p})")).unwrap_or_default();
    if let Some(image) = image {
        body.push_str(&format!("\nFaulted in: {image}{}\n", at(image_path)));
    }
    if let Some(caller) = caller {
        body.push_str(&format!("Called from: {caller}{}\n", at(caller_path)));
    }
    let trace = block(&lines, "Last Exception Backtrace:", TRACE_FRAMES);
    if !trace.is_empty() {
        body.push_str("\nLast Exception Backtrace:\n");
        for line in &trace {
            body.push_str(line.trim());
            body.push('\n');
        }
    }
    if !frames.is_empty() {
        body.push('\n');
        body.push_str(&thread_header(&lines));
        body.push('\n');
        for frame in &frames {
            body.push_str(frame.trim());
            body.push('\n');
        }
    }
    if !dyld.is_empty() {
        body.push_str("\nDyld Error Message:\n");
        for line in &dyld {
            body.push_str(line.trim());
            body.push('\n');
        }
    }
    Summary { verdict, body: body.trim_end().to_string() }
}

/// A frame line's image: `N  image  0xADDR ...`.
fn frame_image(line: &str) -> Option<&str> {
    let mut tokens = line.split_whitespace();
    let number = tokens.next()?;
    let image = tokens.next()?;
    let address = tokens.next()?.strip_prefix("0x")?;
    (number.bytes().all(|b| b.is_ascii_digit())
        && !address.is_empty()
        && address.bytes().take_while(|b| b.is_ascii_hexdigit()).count() > 0)
        .then_some(image)
}

/// The number in `Crashed Thread: N ...`.
fn crashed_index(lines: &[&str]) -> Option<String> {
    let value = field(lines, "Crashed Thread")?;
    let number = value.split(' ').next()?;
    number.parse::<u32>().ok().map(|n| n.to_string())
}

/// The frames of the thread iOS marked as crashed, or else the one `Crashed Thread:` names.
fn crashed_frames<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    let marked = lines.iter().position(|l| l.trim_start().starts_with("Thread ") && l.contains(" Crashed:"));
    let start = marked.or_else(|| {
        let wanted = format!("Thread {}:", crashed_index(lines)?);
        lines.iter().position(|l| l.trim_start().starts_with(&wanted))
    });
    let Some(start) = start else { return Vec::new() };
    let mut out = Vec::new();
    for line in &lines[start + 1..] {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        if !trimmed.starts_with(|c: char| c.is_ascii_digit()) || out.len() == FRAMES {
            break;
        }
        out.push(*line);
    }
    out
}

fn thread_header(lines: &[&str]) -> String {
    if let Some(marked) = lines.iter().find(|l| l.trim_start().starts_with("Thread ") && l.contains(" Crashed:")) {
        return marked.trim().to_string();
    }
    crashed_index(lines)
        .and_then(|n| lines.iter().find(|l| l.trim_start().starts_with(&format!("Thread {n}:"))))
        .map_or_else(|| "Thread".to_string(), |l| l.trim().to_string())
}

/// The `Binary Images:` line for `image`: its full path, which is how a system library is recognised.
fn binary_image_path<'a>(lines: &[&'a str], image: Option<&str>) -> Option<&'a str> {
    let image = image?;
    let start = lines.iter().position(|l| l.trim() == "Binary Images:")?;
    for line in &lines[start + 1..] {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.iter().any(|t| *t == image || t.strip_prefix('+') == Some(image)) {
            return tokens.last().copied().filter(|t| t.starts_with('/'));
        }
    }
    None
}

/// The first line that starts with `key`, with the lines after it up to a blank one, `count` in all.
fn block<'a>(lines: &[&'a str], key: &str, count: usize) -> Vec<&'a str> {
    let Some(start) = lines.iter().position(|l| l.trim() == key || l.trim_start().starts_with(key)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(first) = lines[start].find(key).map(|at| lines[start][at + key.len()..].trim()) {
        if !first.is_empty() {
            out.push(first);
        }
    }
    for line in &lines[start + 1..] {
        if out.len() >= count || line.trim().is_empty() {
            break;
        }
        out.push(*line);
    }
    out
}

fn field<'a>(lines: &[&'a str], key: &str) -> Option<&'a str> {
    let prefix = format!("{key}:");
    lines.iter().find(|l| l.starts_with(&prefix)).map(|l| l[prefix.len()..].trim())
}

/// The word after `marker` in `text`.
fn word_after<'a>(text: &'a str, marker: &str) -> Option<&'a str> {
    text[text.find(marker)? + marker.len()..].split_whitespace().next()
}

/// Whose side the crash is on. The frame that faulted names a binary, and that binary's path says whether it is the
/// app or the emulated system; dyld's message and the exception codes say what kind of failure it was.
#[allow(clippy::too_many_arguments)]
fn verdict(
    process: &str,
    guest: &str,
    image: Option<&str>,
    path: Option<&str>,
    caller: Option<&str>,
    caller_path: Option<&str>,
    dyld: &[&str],
    codes: &str,
    termination: &str,
) -> String {
    let message = dyld.join(" ");
    if let Some(symbol) = word_after(&message, "Symbol not found:") {
        let expected = word_after(&message, "Expected in:").unwrap_or("this iOS");
        let this = if guest.is_empty() { "this one".to_string() } else { format!("iOS {guest}") };
        return format!(
            "The app needs {symbol}, which {expected} doesn't have. It was built for a newer iOS than {this}."
        );
    }
    if let Some(library) = word_after(&message, "Library not loaded:") {
        return format!("The app needs {library}, which this iOS doesn't include.");
    }
    if termination.to_lowercase().contains("codesign") || message.to_lowercase().contains("code signature") {
        return "iOS rejected the app's code signature and closed it. This is not an emulation problem.".into();
    }
    if codes.to_lowercase().contains("0x8badf00d") || message.contains("0x8badf00d") {
        return "iOS closed the app because it took too long to start. The emulator was too slow; the app itself is \
                probably fine."
            .into();
    }
    let null_pointer = ["at 0x0", "at 0x00000000", "KERN_INVALID_ADDRESS", "KERN_PROTECTION_FAILURE"]
        .iter()
        .any(|c| codes.contains(c));
    let Some(image) = image else { return "The crash report doesn't say where the app crashed.".into() };
    if null_pointer && stub(image) {
        if let Some(caller) = caller {
            return format!(
                "The app crashed in {image}, called from {caller}, using something that didn't exist. On a real \
                 device that is a bug in the app; here it can also mean the emulated iOS didn't provide something \
                 the app expected. The system log below may say which."
            );
        }
    }
    let culprit = caller.unwrap_or(image);
    if system_side(process, image, path, caller, caller_path) {
        format!("The app crashed inside {culprit}, part of iOS. The emulation is the likely cause, not the app.")
    } else {
        format!(
            "The app crashed in its own code ({culprit}). The emulation can still be the cause if the app relied on \
             something LightTouch doesn't support yet."
        )
    }
}

/// Whether the frame that failed is a library of the emulated system.
fn system_side(
    process: &str,
    image: &str,
    path: Option<&str>,
    caller: Option<&str>,
    caller_path: Option<&str>,
) -> bool {
    let system = |p: &str| p.starts_with("/System/") || p.starts_with("/usr/lib/");
    if let (Some(_), Some(p)) = (caller, caller_path) {
        return system(p);
    }
    let target = caller.unwrap_or(image);
    let target_path = if caller.is_some() { caller_path } else { path };
    match target_path {
        Some(p) => system(p),
        None => target.ends_with(".dylib") || target != process,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NULL_IN_LIBC: &str = "Incident Identifier: X
Process:         Asphalt8 [123]
OS Version:      iOS 6.1.6 (10B500)
Exception Type:  EXC_BAD_ACCESS (SIGSEGV)
Exception Codes: KERN_INVALID_ADDRESS at 0x00000000
Crashed Thread:  0

Thread 0 name:  Dispatch queue: com.apple.main-thread
Thread 0 Crashed:
0   libsystem_c.dylib             	0x3a8b1234 0x3a8a0000 + 70196
1   Asphalt8                      	0x0012abcd 0x1000 + 1223629
2   Asphalt8                      	0x00100000 0x1000 + 1044480

Thread 1:
0   libsystem_kernel.dylib        	0x3a9b0000 0x3a9a0000 + 65536

Binary Images:
    0x1000 -   0x900fff +Asphalt8 armv7  <abcd> /var/mobile/Applications/X/Asphalt8.app/Asphalt8
0x3a8a0000 - 0x3a90ffff  libsystem_c.dylib armv7  <ef01> /usr/lib/system/libsystem_c.dylib
";

    #[test]
    fn a_null_in_a_runtime_stub_names_the_caller() {
        let s = summarize("Asphalt8_2026-11-01-120000_iPod.plist", NULL_IN_LIBC);
        assert!(s.verdict.starts_with("The app crashed in libsystem_c.dylib, called from Asphalt8"), "{}", s.verdict);
        assert!(s.body.contains("Faulted in: libsystem_c.dylib  (/usr/lib/system/libsystem_c.dylib)"), "{}", s.body);
        assert!(s.body.contains("Called from: Asphalt8  (/var/mobile/Applications/X/Asphalt8.app/Asphalt8)"));
        assert!(s.body.contains("Thread 0 Crashed:\n0   libsystem_c.dylib"));
        assert!(!s.body.contains("libsystem_kernel"), "only the crashed thread's frames");
    }

    #[test]
    fn a_missing_symbol_means_a_newer_ios() {
        let report = "Process: Doodle [1]\nOS Version: iOS 3.1.3 (7E18)\nException Type: EXC_BREAKPOINT\n\n\
                      Dyld Error Message:\n  Symbol not found: _dispatch_main_q\n  Referenced from: /var/x/Doodle\n  \
                      Expected in: /usr/lib/libSystem.B.dylib\n";
        let s = summarize("", report);
        assert_eq!(
            s.verdict,
            "The app needs _dispatch_main_q, which /usr/lib/libSystem.B.dylib doesn't have. It was built for a newer \
             iOS than iOS 3.1.3."
        );
    }

    #[test]
    fn the_engine_log_explains_a_dropped_call() {
        let engine = vec![
            "[gles] REFUSED draw-error:0x500".to_string(),
            "[gles] UNHANDLED glMapBuffer (id 649) -- returning 0".to_string(),
        ];
        assert!(engine_hint(&engine).unwrap().contains("glMapBuffer (id 649)"));
        assert_eq!(engine_hint(&engine[..1]), None, "an app's own GL error is not a dropped call");
    }

    #[test]
    fn no_report_says_the_app_did_not_crash() {
        let text = report(&DeviceLogs::default(), None, Some(""));
        assert!(text.starts_with("There is no crash report"));
        assert!(text.contains("(iOS logged nothing)"));
    }
}
