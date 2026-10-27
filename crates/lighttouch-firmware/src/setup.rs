//! The device's language and region, and Setup Assistant's finished state, seeded into the data volume
//! (SystemEdits/FinishedSetup.swift, Portability/PreferredLocalization.swift). `var_root` is the folder that is
//! /private/var on the device: "" in a data volume's editor, "private/var" in a single-volume system editor.
//!
//! Setup's keys are the ones Setup itself writes on a walk that picks English, the region, Location Services off, no
//! Wi-Fi, a new device, no Apple ID or passcode, the terms and Don't Send: the data volume of each base diffed before
//! and after a walk (n81ap/k48ap 9B206, n90ap 10B329, n90ap 11D257). What the boot's other parts write anyway
//! (lockdown's zone and clock, it_prefs, caches) is left to them.
//!
//! Swift reads locales through Foundation's `Locale` (ICU). Off Apple platforms there is no such thing to lean on, so
//! the few pieces these seeds need are here in plain Rust: a BCP 47 tag to an ICU identifier, a locale's region, and
//! ICU's likely subtags (a language's usual script and region) for the languages iOS has keyboards for. Where they
//! differ from Foundation (a language missing from the table, ICU's deprecated-code aliases), see each function.
use crate::error::Result;
use crate::hfs::Editor;
use crate::keyboards::KeyboardCatalog;
use crate::plistedit;
use plist::{Dictionary, Value};
use std::collections::{HashMap, HashSet};

/// What seed_language chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Language {
    pub language: String,
    pub locale: String,
    pub keyboards: Vec<String>,
}

pub const PURPLE_BUDDY: &str = "mobile/Library/Preferences/com.apple.purplebuddy.plist";
pub const PURPLE_BUDDY_LOCAL: &str = "mobile/Library/Preferences/com.apple.purplebuddy.notbackedup.plist";
pub const LOCATIOND: &str = "mobile/Library/Preferences/com.apple.locationd.plist";
pub const LOCATIOND_LOCAL: &str = "mobile/Library/Preferences/com.apple.locationd.notbackedup.plist";
pub const GLOBAL_PREFERENCES: &str = "mobile/Library/Preferences/.GlobalPreferences.plist";
pub const ROOT_GLOBAL_PREFERENCES: &str = "root/Library/Preferences/.GlobalPreferences.plist";
pub const DATA_ARK: &str = "root/Library/Lockdown/data_ark.plist";

/// The language every prepared device gets when the app names none (the apps always pass one).
pub const DEFAULT_TAG: &str = "en-US";

/// `rel` under the folder that is /private/var (see the module doc), with no leading "/".
pub fn join(var_root: &str, rel: &str) -> String {
    if var_root.is_empty() { rel.to_string() } else { format!("{}/{rel}", var_root.trim_end_matches('/')) }
}

// ---- Locales

/// A locale identifier or BCP 47 tag taken apart: "sr-Latn-RS", "zh_Hant_TW", "en_US_POSIX", "de-DE-u-co-phonebk".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocaleParts {
    /// Lowercase ("en").
    pub language: String,
    /// Titlecase ("Latn").
    pub script: Option<String>,
    /// Uppercase or three digits ("US", "419").
    pub region: Option<String>,
    /// Uppercase ("POSIX").
    pub variants: Vec<String>,
}

/// `id`'s parts, "-" and "_" alike. ICU keywords ("@calendar=...") and BCP 47 extensions ("-u-co-phonebk",
/// "-x-...") are dropped: none of the seeds use them.
pub fn locale_parts(id: &str) -> LocaleParts {
    let id = id.split('@').next().unwrap_or("");
    let mut p = LocaleParts::default();
    let mut subtags = id.split(['-', '_']);
    p.language = subtags.next().unwrap_or("").to_ascii_lowercase();
    for s in subtags {
        if s.is_empty() {
            continue; // "en__POSIX": an empty region
        }
        let alpha = s.chars().all(|c| c.is_ascii_alphabetic());
        let digits = s.chars().all(|c| c.is_ascii_digit());
        if s.len() == 1 {
            break; // an extension singleton: the rest is the extension's
        } else if s.len() == 4 && alpha && p.script.is_none() && p.region.is_none() && p.variants.is_empty() {
            let mut t = s.to_ascii_lowercase();
            t[..1].make_ascii_uppercase();
            p.script = Some(t);
        } else if p.region.is_none() && p.variants.is_empty() && ((s.len() == 2 && alpha) || (s.len() == 3 && digits)) {
            p.region = Some(s.to_ascii_uppercase());
        } else {
            p.variants.push(s.to_ascii_uppercase());
        }
    }
    p
}

/// A locale's region ("US" of "en_US", "RS" of "sr_Latn_RS"); none where it names none (Foundation's
/// `Locale(identifier:).region`, which doesn't guess one either).
pub fn locale_region(id: &str) -> Option<String> {
    locale_parts(id).region
}

/// A BCP 47 tag as ICU names the locale (`Locale.identifier(.icu, from:)`): "en-US" to "en_US", "zh-Hant-TW" to
/// "zh_Hant_TW", "en-US-POSIX" to "en_US_POSIX", "de-1996" to "de__1996". Extensions and keywords are left out, as
/// deviceLocale drops them anyway. Unlike ICU, deprecated language codes ("iw", "in") are not renamed: the apps pass
/// the current codes (Android's `toLanguageTag()` already maps them).
pub fn icu_identifier(tag: &str) -> String {
    let p = locale_parts(tag);
    let mut out = p.language.clone();
    if let Some(s) = &p.script {
        out += "_";
        out += s;
    }
    if let Some(r) = &p.region {
        out += "_";
        out += r;
    }
    if !p.variants.is_empty() {
        if p.region.is_none() {
            out += "_";
        }
        for v in &p.variants {
            out += "_";
            out += v;
        }
    }
    out
}

/// ICU's likely subtags (CLDR likelySubtags) for the languages iOS 1 to 7 ship keyboards or localizations for: each
/// language's usual script and region. A language missing here stays as it is (ICU would still know it): only the
/// keyboard choice for a firmware without its own defaults (before 5.x) uses this, and both sides of that comparison
/// go through it, so a missing entry still groups a language's keyboards together.
const LIKELY: &[(&str, &str, &str)] = &[
    ("af", "Latn", "ZA"), ("am", "Ethi", "ET"), ("ar", "Arab", "EG"), ("as", "Beng", "IN"), ("az", "Latn", "AZ"),
    ("be", "Cyrl", "BY"), ("bg", "Cyrl", "BG"), ("bn", "Beng", "BD"), ("bo", "Tibt", "CN"), ("br", "Latn", "FR"),
    ("bs", "Latn", "BA"), ("ca", "Latn", "ES"), ("chr", "Cher", "US"), ("cs", "Latn", "CZ"), ("cy", "Latn", "GB"),
    ("da", "Latn", "DK"), ("de", "Latn", "DE"), ("dz", "Tibt", "BT"), ("el", "Grek", "GR"), ("en", "Latn", "US"),
    ("eo", "Latn", "001"), ("es", "Latn", "ES"), ("et", "Latn", "EE"), ("eu", "Latn", "ES"), ("fa", "Arab", "IR"),
    ("fi", "Latn", "FI"), ("fil", "Latn", "PH"), ("fo", "Latn", "FO"), ("fr", "Latn", "FR"), ("ga", "Latn", "IE"),
    ("gd", "Latn", "GB"), ("gl", "Latn", "ES"), ("gu", "Gujr", "IN"), ("haw", "Latn", "US"), ("he", "Hebr", "IL"),
    ("hi", "Deva", "IN"), ("hr", "Latn", "HR"), ("hu", "Latn", "HU"), ("hy", "Armn", "AM"), ("id", "Latn", "ID"),
    ("is", "Latn", "IS"), ("it", "Latn", "IT"), ("iu", "Cans", "CA"), ("ja", "Jpan", "JP"), ("ka", "Geor", "GE"),
    ("kk", "Cyrl", "KZ"), ("km", "Khmr", "KH"), ("kn", "Knda", "IN"), ("ko", "Kore", "KR"), ("ku", "Latn", "TR"),
    ("ky", "Cyrl", "KG"), ("lb", "Latn", "LU"), ("lo", "Laoo", "LA"), ("lt", "Latn", "LT"), ("lv", "Latn", "LV"),
    ("mi", "Latn", "NZ"), ("mk", "Cyrl", "MK"), ("ml", "Mlym", "IN"), ("mn", "Cyrl", "MN"), ("mr", "Deva", "IN"),
    ("ms", "Latn", "MY"), ("mt", "Latn", "MT"), ("my", "Mymr", "MM"), ("nb", "Latn", "NO"), ("ne", "Deva", "NP"),
    ("nl", "Latn", "NL"), ("nn", "Latn", "NO"), ("no", "Latn", "NO"), ("or", "Orya", "IN"), ("pa", "Guru", "IN"),
    ("pl", "Latn", "PL"), ("ps", "Arab", "AF"), ("pt", "Latn", "BR"), ("ro", "Latn", "RO"), ("ru", "Cyrl", "RU"),
    ("sa", "Deva", "IN"), ("si", "Sinh", "LK"), ("sk", "Latn", "SK"), ("sl", "Latn", "SI"), ("sq", "Latn", "AL"),
    ("sr", "Cyrl", "RS"), ("sv", "Latn", "SE"), ("sw", "Latn", "TZ"), ("ta", "Taml", "IN"), ("te", "Telu", "IN"),
    ("tg", "Cyrl", "TJ"), ("th", "Thai", "TH"), ("tk", "Latn", "TM"), ("to", "Latn", "TO"), ("tr", "Latn", "TR"),
    ("ug", "Arab", "CN"), ("uk", "Cyrl", "UA"), ("ur", "Arab", "PK"), ("uz", "Latn", "UZ"), ("vi", "Latn", "VN"),
    ("yue", "Hant", "HK"), ("zh", "Hans", "CN"), ("zu", "Latn", "ZA"),
];

/// The language with its script and region filled in as ICU's maximalIdentifier fills them ("en" to en-Latn-US,
/// "zh_TW" to zh-Hant-TW, "zh-Hant" to zh-Hant-TW): (language, script, region).
pub fn maximize(id: &str) -> (String, Option<String>, Option<String>) {
    let p = locale_parts(id);
    let l = p.language.as_str();
    let region = p.region.as_deref();
    // the languages whose script follows the region (or whose region follows the script)
    let special: Option<(&str, &str)> = match (l, p.script.as_deref(), region) {
        ("zh", None, Some("TW" | "HK" | "MO")) => Some(("Hant", region.unwrap())),
        ("zh", Some("Hant"), None) => Some(("Hant", "TW")),
        ("yue", None, Some("CN")) | ("yue", Some("Hans"), None) => Some(("Hans", "CN")),
        ("sr", Some("Latn"), None) => Some(("Latn", "RS")),
        ("pa", None, Some("PK")) | ("pa", Some("Arab"), None) => Some(("Arab", "PK")),
        ("az", None, Some("IR")) => Some(("Arab", "IR")),
        ("uz", None, Some("AF")) | ("uz", Some("Arab"), None) => Some(("Arab", "AF")),
        _ => None,
    };
    let likely = LIKELY.iter().find(|(lang, _, _)| *lang == l).map(|&(_, s, r)| (s, r));
    let (s, r) = match (special, likely) {
        (Some(x), _) => (Some(x.0), Some(x.1)),
        (None, Some(x)) => (Some(x.0), Some(x.1)),
        (None, None) => (None, None),
    };
    let script = p.script.clone().or(s.map(str::to_string));
    let region = p.region.clone().or(r.map(str::to_string));
    (p.language, script, region)
}

// ---- The device's language (PreferredLocalization)

/// "zh_Hant-TW" to "zh-hant-tw": the form both sides are compared in.
fn key(tag: &str) -> String {
    tag.replace('_', "-").to_lowercase()
}

fn parts(tag: &str) -> (String, Option<String>, Option<String>) {
    let k = key(tag);
    let p: Vec<&str> = k.split('-').filter(|s| !s.is_empty()).collect();
    let mut language = p.first().copied().unwrap_or("").to_string();
    language = match language.as_str() {
        "no" => "nb",
        "iw" => "he",
        "in" => "id",
        "ji" => "yi",
        other => other,
    }
    .to_string();
    let (mut script, mut region): (Option<String>, Option<String>) = (None, None);
    for x in p.iter().skip(1) {
        let n = x.chars().count();
        if n == 4 && script.is_none() {
            script = Some(x.to_string());
        } else if region.is_none() && (n == 2 || n == 3) {
            region = Some(x.to_string());
        }
    }
    if language == "zh" && script.is_none() {
        let hant = matches!(region.as_deref(), Some("tw" | "hk" | "mo"));
        script = Some(if hant { "hant" } else { "hans" }.to_string());
    }
    (language, script, region)
}

/// The tags to try for `tag`, most specific first.
fn chain(tag: &str) -> Vec<String> {
    let (l, s, r) = parts(tag);
    let mut out: Vec<String> = Vec::new();
    let mut add = |t: String| {
        if !out.contains(&t) {
            out.push(t);
        }
    };
    if let (Some(s), Some(r)) = (&s, &r) {
        add(format!("{l}-{s}-{r}"));
    }
    if let Some(s) = &s {
        add(format!("{l}-{s}"));
    }
    if let Some(r) = &r {
        add(format!("{l}-{r}"));
    }
    // Apple's parent locales: English outside the US goes to British English, Spanish in the Americas to Latin
    // American Spanish.
    if let Some(r) = &r {
        if l == "en" && !["us", "as", "gu", "mp", "pr", "um", "vi"].contains(&r.as_str()) {
            add("en-gb".to_string());
        }
        if l == "es" && r != "es" {
            add("es-419".to_string());
        }
    }
    add(l);
    out
}

/// Bundle.preferredLocalizations(from:forPreferences:) as FirmwareKit does it off Apple platforms: the first
/// preferred language that `supported` has, by the fallbacks Apple's matcher takes (zh-HK to zh-Hant, en-AU to
/// en-GB, es-MX to es-419 then es, no to nb, pt-BR to pt then any pt), else the next preferred language.
pub fn preferred_localization(supported: &[String], preferred: &[String]) -> Option<String> {
    let mut names: HashMap<String, &String> = HashMap::new();
    for s in supported {
        names.entry(key(s)).or_insert(s);
    }
    for tag in preferred {
        for candidate in chain(tag) {
            if let Some(hit) = names.get(&candidate) {
                return Some((*hit).clone());
            }
        }
        // any variant of the language, as Apple's matcher gives pt-PT for pt-BR when there is no pt
        let lang = parts(tag).0;
        if let Some(hit) = supported.iter().find(|s| parts(s).0 == lang) {
            return Some(hit.clone());
        }
    }
    None
}

/// The device's language: the first of `preferred` (BCP 47 tags) that `supported`, the firmware's own
/// AppleLanguages, has, matched as a bundle picks its localization ("zh-Hans-CN" to "zh-Hans", "pt-BR" to "pt",
/// "en-AU" to "en-GB" where the firmware has it), else English. Its AppleLocale: `locale` ("uk_UA") where `formats`,
/// the firmware's region formats, has it, whatever the language, as Region Format sets it; else the language's code
/// with `region` ("fr_FR").
pub fn device_language(
    supported: &[String],
    preferred: &[String],
    region: Option<&str>,
    locale: Option<&str>,
    formats: Option<&HashSet<String>>,
) -> (String, String) {
    let language = preferred_localization(supported, preferred).unwrap_or_else(|| "en".to_string());
    if let (Some(locale), Some(formats)) = (locale, formats)
        && formats.contains(locale)
    {
        return (language, locale.to_string());
    }
    let code = match locale_parts(&language).language.as_str() {
        "" => "en".to_string(),
        c => c.to_string(),
    };
    let locale = match region {
        Some(r) => format!("{code}_{r}"),
        None => code,
    };
    (language, locale)
}

/// The locale to seed as AppleLocale, as ICU names it ("uk_UA", "sr_Latn_RS"), without keywords: `tag`'s, else
/// en-US's. (Swift falls back to the Mac's own AppleLocale; the apps always pass a tag.)
pub fn device_locale(tag: Option<&str>) -> String {
    icu_identifier(tag.unwrap_or(DEFAULT_TAG))
}

/// The region formats the firmware offers (Settings > International > Region Format, CFLocale's available locales):
/// the locales in its ICU data, the table of contents of /usr/share/icu/icudt*l.dat ("uk_UA" for icudt42l/uk_UA.res);
/// None where it has none.
pub fn region_formats(system: &Editor) -> Option<HashSet<String>> {
    let names = system.list("usr/share/icu").ok()?;
    let name = names.iter().find(|n| n.starts_with("icudt") && n.ends_with("l.dat"))?;
    let data = system.read(&format!("usr/share/icu/{name}")).ok()?;
    icu_locales(&data)
}

/// The top-level .res entries of an ICU common data file. Its layout: a header (its size, magic DA 27, format
/// "CmnD"), then the entry count and each entry's name and data offsets, both from the header's end.
pub fn icu_locales(b: &[u8]) -> Option<HashSet<String>> {
    let u16le = |at: usize| u16::from_le_bytes([b[at], b[at + 1]]) as usize;
    let u32le = |at: usize| u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]) as usize;
    if b.len() <= 24 || b[2] != 0xDA || b[3] != 0x27 || &b[12..16] != b"CmnD" {
        return None;
    }
    let toc = u16le(0);
    if toc + 4 > b.len() {
        return None;
    }
    let count = u32le(toc);
    if toc + 4 + count * 8 > b.len() {
        return None;
    }
    let mut out = HashSet::new();
    for i in 0..count {
        let at = toc + u32le(toc + 4 + i * 8);
        if at >= b.len() {
            return None;
        }
        let end = at + b[at..].iter().position(|&c| c == 0)?;
        // a locale is a top-level .res ("icudt42l/uk_UA.res"); coll/ and the like are not region formats
        let path = String::from_utf8_lossy(&b[at..end]).into_owned();
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if parts.len() == 2
            && let Some(locale) = parts[1].strip_suffix(".res")
        {
            out.insert(locale.to_string());
        }
    }
    Some(out)
}

/// The language (BCP 47 `tag`, the device's region with it) first in each user's .GlobalPreferences that lists
/// the firmware's languages, AppleLocale set, AppleKeyboards from `catalog`. None where the firmware has no list.
/// `keyboards`: `create --keyboards`, Mac input source IDs, of which off the Mac only Emoji & Symbols has an iOS
/// counterpart (see KeyboardCatalog::keyboards). With `formats`, AppleLocale is `tag`'s own locale where the firmware
/// offers it as a region format, whatever the language.
pub fn seed_language(
    data: &mut Editor,
    var_root: &str,
    tag: Option<&str>,
    keyboards: Option<&[String]>,
    catalog: Option<&KeyboardCatalog>,
    formats: Option<&HashSet<String>>,
) -> Result<Option<Language>> {
    let tag = tag.unwrap_or(DEFAULT_TAG);
    let preferred = [tag.to_string()];
    let region = locale_region(tag);
    let locale = device_locale(Some(tag));
    let extra: &[String] = keyboards.unwrap_or(&[]);
    let mut chosen: Option<Language> = None;
    for rel in [GLOBAL_PREFERENCES, ROOT_GLOBAL_PREFERENCES] {
        let path = join(var_root, rel);
        if !data.exists(&path) {
            continue;
        }
        plistedit::rewrite(data, &path, |d| {
            let Some(list) = string_array(d.get("AppleLanguages")) else { return Ok(()) };
            let (language, locale) = device_language(&list, &preferred, region.as_deref(), Some(&locale), formats);
            let mut languages = vec![language.clone()];
            languages.extend(list.into_iter().filter(|l| *l != language));
            d.insert("AppleLanguages".into(), plistedit::strings(&languages));
            d.insert("AppleLocale".into(), Value::String(locale.clone()));
            let kb = catalog.map(|c| c.keyboards(&language, &locale, extra)).unwrap_or_default();
            if !kb.is_empty() {
                d.insert("AppleKeyboards".into(), plistedit::strings(&kb));
            }
            if chosen.is_none() {
                chosen = Some(Language { language, locale, keyboards: kb });
            }
            Ok(())
        })?;
    }
    Ok(chosen)
}

/// A plist array whose items are all strings (Swift's `as? [String]`).
pub(crate) fn string_array(v: Option<&Value>) -> Option<Vec<String>> {
    v?.as_array()?.iter().map(|s| s.as_string().map(str::to_string)).collect()
}

/// What Setup leaves on iOS `major` (5, 6, 7) after a walk in `language` and `locale` ("en", "en_AU"), finished at
/// `now` (Unix seconds): each plist under /private/var and its keys. Empty before 5 (iTunes activated those, no
/// Setup Assistant).
pub fn finished_setup(major: u32, language: &str, locale: &str, now: i64) -> Vec<(String, Dictionary)> {
    if major < 5 {
        return Vec::new();
    }
    let t = || Value::Boolean(true);
    let mut buddy = plistedit::dictionary([
        ("SetupDone", t()),
        ("SetupFinishedAllSteps", t()),
        ("RestoreChoice", t()),
        ("PBTCPresented", t()),
        ("PBDiagnosticsPresented", t()),
        ("WiFiPresented", t()),
        ("Language", language.into()),
        ("Locale", locale.into()),
    ]);
    let off: Dictionary;
    let buddy_local: Dictionary;
    let ark: Dictionary;
    let mut extra: Vec<(String, Dictionary)> = Vec::new();
    match major {
        5 | 6 => {
            buddy.insert(if major == 5 { "AppleIDPresented" } else { "AppleIDPB3Presented" }.into(), t());
            if major == 6 {
                buddy.insert("SetupVersion".into(), 3.into());
            }
            buddy_local = plistedit::dictionary([("LocationServicesPresented", t())]);
            off = plistedit::dictionary([("LocationServicesEnabled", 0.into())]);
            ark = plistedit::dictionary([
                ("com.apple.purplebuddy-SetupState", "SetupUsingAssistant".into()),
                ("com.apple.mobile.user_preferences-UserSetLanguage", t()),
                ("com.apple.mobile.user_preferences-UserSetLocale", t()),
            ]);
        }
        _ => {
            for (k, v) in [
                ("AppleIDPB5Presented", t()),
                ("PasscodePresented", t()),
                ("SetupVersion", 5.into()),
                ("SetupState", "SetupUsingAssistant".into()),
                ("AppleIDForceUpgrade", false.into()),
            ] {
                buddy.insert(k.into(), v);
            }
            buddy_local = plistedit::dictionary([("LocationServices2Presented", t()), ("CloudConfigPresented", t())]);
            off = plistedit::dictionary([("LocationServicesEnabledIn7.0", 0.into())]);
            ark = plistedit::dictionary([
                ("-ActivationStateAcknowledged", t()),
                ("com.apple.mobile.chaperone-NotSoFresh", t()),
                ("-FirstPurpleBuddyCompletion", now.into()),
            ]);
            // Setup's cloud configuration step, through ManagedConfiguration: without the record, 7.x's
            // PSSetupAssistantNeedsToRun brings Setup back for it (Wi-Fi, then Welcome) on every boot.
            extra.push((
                "mobile/Library/ConfigurationProfiles/CloudConfigurationDetails.plist".into(),
                plistedit::dictionary([
                    ("AllowPairing", t()),
                    ("CloudConfigurationUIComplete", t()),
                    ("IsSupervised", false.into()),
                    ("PostSetupProfileWasInstalled", false.into()),
                ]),
            ));
            extra.push((
                "mobile/Library/Preferences/com.apple.mobile.user_preferences.plist".into(),
                plistedit::dictionary([("UserSetLanguage", t()), ("UserSetLocale", t())]),
            ));
        }
    }
    let mut out = vec![
        (PURPLE_BUDDY.to_string(), buddy),
        (PURPLE_BUDDY_LOCAL.to_string(), buddy_local),
        (LOCATIOND.to_string(), off.clone()),
        (LOCATIOND_LOCAL.to_string(), off),
        (DATA_ARK.to_string(), ark),
    ];
    out.extend(extra);
    out
}

/// Setup Assistant's finished state for iOS `major` (5, 6, 7) in `language`/`locale`, dated `now` (Unix seconds):
/// `finished_setup` merged into the plists already there (new ones XML), as Setup's walk leaves it.
pub fn seed_finished_setup(data: &mut Editor, var_root: &str, major: u32, language: &str, locale: &str, now: i64) -> Result<()> {
    for (rel, keys) in finished_setup(major, language, locale, now) {
        plistedit::seed(data, &join(var_root, &rel), |d| {
            for (k, v) in keys {
                d.insert(k, v);
            }
            Ok(())
        })?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::keyboards::tests::catalog_712;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// A scratch HFS+ volume for the seeds, removed when dropped.
    pub(crate) struct Scratch(pub std::path::PathBuf);
    impl Scratch {
        pub(crate) fn new(name: &str) -> (Scratch, Editor) {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static N: AtomicUsize = AtomicUsize::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("lt-seed-{name}-{}-{n}.img", std::process::id()));
            Editor::format(&path, 16 << 20, "Scratch", false, 1_700_000_000).unwrap();
            let ed = Editor::open(&path).unwrap();
            (Scratch(path), ed)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    pub(crate) fn put_plist(ed: &mut Editor, path: &str, d: Dictionary, format: plistedit::Format) {
        if let Some((dir, _)) = path.rsplit_once('/') {
            ed.mkdirs(dir).unwrap();
        }
        ed.write(path, plistedit::serialize(&Value::Dictionary(d), format).unwrap(), None).unwrap();
    }

    pub(crate) fn get_plist(ed: &Editor, path: &str) -> Dictionary {
        plistedit::read(ed, path).unwrap().unwrap_or_default()
    }

    #[test]
    fn locales() {
        assert_eq!(icu_identifier("en-US"), "en_US");
        assert_eq!(icu_identifier("zh-Hant-TW"), "zh_Hant_TW");
        assert_eq!(icu_identifier("sr-latn-rs"), "sr_Latn_RS");
        assert_eq!(icu_identifier("de-DE-u-co-phonebk"), "de_DE");
        assert_eq!(icu_identifier("en-US-POSIX"), "en_US_POSIX");
        assert_eq!(icu_identifier("es-419"), "es_419");
        assert_eq!(icu_identifier("fr"), "fr");
        assert_eq!(locale_region("en_US"), Some("US".into()));
        assert_eq!(locale_region("zh_Hans"), None);
        assert_eq!(locale_region("sr_Latn_RS@calendar=gregorian"), Some("RS".into()));
        assert_eq!(maximize("en"), ("en".into(), Some("Latn".into()), Some("US".into())));
        assert_eq!(maximize("zh_TW").1.as_deref(), Some("Hant"));
        assert_eq!(maximize("zh-Hant").2.as_deref(), Some("TW"));
        assert_eq!(maximize("zh-Hans").1.as_deref(), Some("Hans"));
        assert_eq!(maximize("fr_CA"), ("fr".into(), Some("Latn".into()), Some("CA".into())));
        // the Swift test's deviceLocale cases (an explicit language)
        assert_eq!(device_locale(Some("fr-CA")), "fr_CA");
        assert_eq!(device_locale(None), "en_US");
    }

    /// deviceLanguageIsTheFirstPreferredOneTheFirmwareHas: AppleLanguages as each rootfs ships it.
    #[test]
    fn device_language_is_the_first_preferred_one_the_firmware_has() {
        let v114 = v(&[
            "en", "fr", "de", "ja", "nl", "it", "es", "pt-PT", "da", "fi", "nb", "sv", "ko", "zh-Hans", "zh-Hant", "ru",
            "pl",
        ]);
        let mut v313: Vec<String> = v114.iter().filter(|l| *l != "pt-PT").cloned().collect();
        v313.extend(v(&["pt", "pt-PT", "tr", "uk", "ar", "hr", "cs", "el", "he", "ro", "sk", "th", "id", "ms"]));
        let mut v421 = v313.clone();
        v421.extend(v(&["en-GB", "ca", "hu", "vi"]));
        let mut v712 = v421.clone();
        v712.extend(v(&["es-MX", "en-AU"]));
        let pick = |list: &[String], preferred: &[&str], region: Option<&str>| {
            let (l, locale) = device_language(list, &v(preferred), region, None, None);
            if region.is_none() { l } else { format!("{l} {locale}") }
        };
        assert_eq!(pick(&v421, &["de-DE"], Some("DE")), "de de_DE");
        assert_eq!(pick(&v421, &["fr-CA"], Some("CA")), "fr fr_CA");
        assert_eq!(pick(&v421, &["zh-Hans-CN"], Some("CN")), "zh-Hans zh_CN");
        assert_eq!(pick(&v421, &["zh-Hant-TW"], None), "zh-Hant");
        assert_eq!(pick(&v421, &["zh-HK"], None), "zh-Hant");
        assert_eq!(pick(&v421, &["pt-BR"], None), "pt");
        assert_eq!(pick(&v421, &["pt-PT"], None), "pt-PT");
        assert_eq!(pick(&v114, &["pt-BR"], None), "pt-PT");
        assert_eq!(pick(&v421, &["nb-NO"], None), "nb");
        assert_eq!(pick(&v421, &["no"], None), "nb");
        assert_eq!(pick(&v421, &["en-GB"], Some("GB")), "en-GB en_GB");
        assert_eq!(pick(&v313, &["en-GB"], None), "en");
        assert_eq!(pick(&v712, &["en-AU"], None), "en-AU");
        assert_eq!(pick(&v421, &["en-AU"], None), "en-GB");
        assert_eq!(pick(&v421, &["en-US"], None), "en");
        assert_eq!(pick(&v712, &["es-MX"], None), "es-MX");
        assert_eq!(pick(&v421, &["es-MX"], None), "es");
        assert_eq!(pick(&v421, &["es-419"], None), "es");
        assert_eq!(pick(&v421, &["vi-VN", "fr-FR"], None), "vi", "down the list");
        assert_eq!(pick(&v313, &["vi-VN", "fr-FR"], None), "fr", "down the list");
        assert_eq!(pick(&v421, &["hi-IN"], Some("IN")), "en en_IN", "else English");
        assert_eq!(pick(&v313, &["ca-ES"], None), "en", "else English");
        assert_eq!(device_language(&v421, &v(&["fr"]), None, None, None).1, "fr");

        // seedLanguage: the firmware's list with the language first, and the region
        let (_s, mut ed) = Scratch::new("lang");
        let mut d = Dictionary::new();
        d.insert("AppleLanguages".into(), plistedit::strings(&v421));
        d.insert("AppleLocale".into(), "en_US".into());
        put_plist(&mut ed, &join("private/var", GLOBAL_PREFERENCES), d, plistedit::Format::Binary);
        seed_language(&mut ed, "private/var", Some("de-DE"), None, None, None).unwrap();
        let g = get_plist(&ed, &join("private/var", GLOBAL_PREFERENCES));
        let mut want = vec!["de".to_string()];
        want.extend(v421.iter().filter(|l| *l != "de").cloned());
        assert_eq!(string_array(g.get("AppleLanguages")), Some(want));
        assert_eq!(g.get("AppleLocale").and_then(Value::as_string), Some("de_DE"));
        let bytes = ed.read(&join("private/var", GLOBAL_PREFERENCES)).unwrap();
        assert_eq!(plistedit::format_of(&bytes), plistedit::Format::Binary, "the stock format is kept");
    }

    /// An ICU common data file (icudt42l.dat's shape) whose table of contents names `entries`.
    fn icu_data(entries: &[&str]) -> Vec<u8> {
        let mut d = vec![32u8, 0, 0xDA, 0x27, 20, 0, 0, 0, 0, 0, 2, 0];
        d.extend_from_slice(b"CmnD");
        d.extend_from_slice(&[0u8; 16]);
        d.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        let mut names = Vec::new();
        for e in entries {
            d.extend_from_slice(&((4 + entries.len() * 8 + names.len()) as u32).to_le_bytes());
            d.extend_from_slice(&0u32.to_le_bytes());
            names.extend_from_slice(e.as_bytes());
            names.push(0);
        }
        d.extend(names);
        d
    }

    /// deviceLocaleIsTheMacsWhereTheFirmwareHasTheRegionFormat (the tag's locale here).
    #[test]
    fn device_locale_is_the_tags_where_the_firmware_has_the_region_format() {
        let (_s, mut ed) = Scratch::new("icu");
        ed.mkdirs("usr/share/icu").unwrap();
        assert_eq!(region_formats(&ed), None, "no ICU data");
        ed.write(
            "usr/share/icu/icudt42l.dat",
            icu_data(&[
                "icudt42l/res_index.res", "icudt42l/en.res", "icudt42l/en_US.res", "icudt42l/uk.res",
                "icudt42l/uk_UA.res", "icudt42l/sr_Latn_RS.res", "icudt42l/coll/hi_IN.res", "icudt42l/cnvalias.icu",
            ]),
            None,
        )
        .unwrap();
        let formats = region_formats(&ed).unwrap();
        for f in ["en", "en_US", "uk", "uk_UA", "sr_Latn_RS"] {
            assert!(formats.contains(f), "{f}");
        }
        assert!(!formats.contains("hi_IN") && !formats.contains("cnvalias.icu"), "only top-level locales");

        let v322 = v(&["en", "fr", "de", "ja", "nl", "it", "es", "zh-Hans", "ru"]);
        let pick = |preferred: &str, region: &str, locale: &str| {
            let (l, loc) = device_language(&v322, &v(&[preferred]), Some(region), Some(locale), Some(&formats));
            format!("{l} {loc}")
        };
        assert_eq!(pick("uk-UA", "UA", "uk_UA"), "en uk_UA");
        assert_eq!(pick("ru-UA", "UA", "ru_UA"), "ru ru_UA", "a firmware without the format: language and region");
        assert_eq!(pick("hi-IN", "IN", "hi_IN"), "en en_IN");
        assert_eq!(pick("fr-FR", "FR", "fr_FR"), "fr fr_FR");
        assert_eq!(device_language(&v322, &v(&["uk-UA"]), Some("UA"), None, None).1, "en_UA");

        let mut d = Dictionary::new();
        d.insert("AppleLanguages".into(), plistedit::strings(&v322));
        d.insert("AppleLocale".into(), "en_US".into());
        put_plist(&mut ed, &join("var", GLOBAL_PREFERENCES), d, plistedit::Format::Binary);
        let seeded = seed_language(&mut ed, "var", Some("uk-UA"), None, None, Some(&formats)).unwrap().unwrap();
        assert_eq!((seeded.language.as_str(), seeded.locale.as_str()), ("en", "uk_UA"));
        let g = get_plist(&ed, &join("var", GLOBAL_PREFERENCES));
        assert_eq!(g.get("AppleLocale").and_then(Value::as_string), Some("uk_UA"));
        assert_eq!(string_array(g.get("AppleLanguages")), Some(v322.clone()));
    }

    /// keyboardsAreTheLanguagesThenTheMacsTheFirmwareHas, its seedLanguage part: AppleKeyboards beside the language.
    #[test]
    fn seed_language_writes_the_keyboards() {
        let (_s, mut ed) = Scratch::new("kb");
        let mut d = Dictionary::new();
        d.insert("AppleLanguages".into(), plistedit::strings(&["en", "fr", "ja"]));
        d.insert("AppleLocale".into(), "en_US".into());
        put_plist(&mut ed, GLOBAL_PREFERENCES, d, plistedit::Format::Binary);
        let catalog = catalog_712();
        let seeded = seed_language(&mut ed, "", Some("ja-JP"), Some(&[]), Some(&catalog), None).unwrap().unwrap();
        let g = get_plist(&ed, GLOBAL_PREFERENCES);
        assert_eq!(string_array(g.get("AppleKeyboards")), Some(seeded.keyboards.clone()));
        assert_eq!(seeded.keyboards.first().map(String::as_str), Some("ja_JP-Kana"));
        assert_eq!(seeded.locale, "ja_JP");
    }

    /// finishedSetupSeedsWhatSetupWrites: the keys Setup itself left on walked devices, merged with what is there.
    #[test]
    fn finished_setup_seeds_what_setup_writes() {
        for major in [4u32, 5, 6, 7] {
            let (_s, mut ed) = Scratch::new("setup");
            let mut d = Dictionary::new();
            d.insert("AppleLanguages".into(), plistedit::strings(&["en", "fr", "de"]));
            d.insert("AppleLocale".into(), "en_US".into());
            put_plist(&mut ed, GLOBAL_PREFERENCES, d, plistedit::Format::Binary);
            let l = seed_language(&mut ed, "", Some("fr-CA"), None, None, None).unwrap().unwrap();
            seed_finished_setup(&mut ed, "", major, &l.language, &l.locale, 1_700_000_000).unwrap();
            let keys = |path: &str| -> HashSet<String> { get_plist(&ed, path).keys().cloned().collect() };
            let set = |items: &[&str]| -> HashSet<String> { items.iter().map(|s| s.to_string()).collect() };
            let mobile = "mobile/Library/Preferences/";
            if major < 5 {
                assert!(keys(PURPLE_BUDDY).is_empty());
                assert!(!ed.exists(PURPLE_BUDDY));
                continue;
            }
            let mut buddy = set(&[
                "SetupDone", "SetupFinishedAllSteps", "RestoreChoice", "PBTCPresented", "PBDiagnosticsPresented",
                "WiFiPresented", "Language", "Locale",
            ]);
            buddy.extend(match major {
                5 => set(&["AppleIDPresented"]),
                6 => set(&["AppleIDPB3Presented", "SetupVersion"]),
                _ => set(&["AppleIDPB5Presented", "PasscodePresented", "SetupVersion", "SetupState", "AppleIDForceUpgrade"]),
            });
            assert_eq!(keys(PURPLE_BUDDY), buddy, "{major}");
            let b = get_plist(&ed, PURPLE_BUDDY);
            let version = b.get("SetupVersion").and_then(Value::as_signed_integer);
            assert_eq!(version, match major { 6 => Some(3), 7 => Some(5), _ => None });
            assert_eq!(b.get("Locale").and_then(Value::as_string), Some("fr_CA"));
            assert_eq!(
                keys(PURPLE_BUDDY_LOCAL),
                if major == 7 { set(&["LocationServices2Presented", "CloudConfigPresented"]) } else { set(&["LocationServicesPresented"]) }
            );
            let off = if major == 7 { "LocationServicesEnabledIn7.0" } else { "LocationServicesEnabled" };
            for name in ["com.apple.locationd.plist", "com.apple.locationd.notbackedup.plist"] {
                let p = get_plist(&ed, &format!("{mobile}{name}"));
                assert_eq!(p.get(off).and_then(Value::as_signed_integer), Some(0), "{name}: Location Services off");
            }
            let cloud = get_plist(&ed, "mobile/Library/ConfigurationProfiles/CloudConfigurationDetails.plist");
            assert_eq!(cloud.get("CloudConfigurationUIComplete").and_then(Value::as_boolean), if major == 7 { Some(true) } else { None });
            assert_eq!(
                keys(&format!("{mobile}com.apple.mobile.user_preferences.plist")),
                if major == 7 { set(&["UserSetLanguage", "UserSetLocale"]) } else { set(&[]) }
            );
            let g = get_plist(&ed, GLOBAL_PREFERENCES);
            assert_eq!(g.get("AppleLocale").and_then(Value::as_string), Some("fr_CA"));
            assert_eq!(string_array(g.get("AppleLanguages")), Some(v(&["fr", "en", "de"])));
            assert_eq!(
                keys(DATA_ARK),
                if major == 7 {
                    set(&["-ActivationStateAcknowledged", "com.apple.mobile.chaperone-NotSoFresh", "-FirstPurpleBuddyCompletion"])
                } else {
                    set(&[
                        "com.apple.purplebuddy-SetupState",
                        "com.apple.mobile.user_preferences-UserSetLanguage",
                        "com.apple.mobile.user_preferences-UserSetLocale",
                    ])
                }
            );
            if major == 7 {
                let ark = get_plist(&ed, DATA_ARK);
                assert_eq!(ark.get("-FirstPurpleBuddyCompletion").and_then(Value::as_signed_integer), Some(1_700_000_000));
            }
        }
    }
}
