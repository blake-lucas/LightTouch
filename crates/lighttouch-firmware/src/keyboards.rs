//! The keyboards a firmware has (its TextInput bundles' input modes and, from 5.x, each language's defaults) and the
//! AppleKeyboards a prepared device gets for its language (SystemEdits/Keyboards.swift). Off the Mac there are no
//! Text Input Sources: the device gets its language's keyboards and those `create --keyboards` names, as layouts.
use crate::hfs::Editor;
use crate::plistedit;
use crate::setup::{locale_region, maximize, string_array};
use plist::{Dictionary, Value};
use std::collections::BTreeMap;

/// The Mac's Emoji & Symbols palette, the one Mac input source that maps to an iOS keyboard without its languages.
pub const EMOJI_PALETTE: &str = "com.apple.CharacterPaletteIM";

/// One of the Mac's input sources (Text Input Sources): its ID, languages, localized name, and whether it is a
/// keyboard layout (com.apple.keylayout.French) rather than an input method's mode or the Emoji & Symbols palette.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MacKeyboard {
    pub id: String,
    pub languages: Vec<String>,
    pub name: String,
    pub layout: bool,
}

/// The Mac input sources `ids` (`create --keyboards`) as this host knows them. There are no Text Input Sources off
/// the Mac, so each is a layout with no languages: only Emoji & Symbols finds an iOS keyboard that way.
pub fn mac_keyboards(ids: Option<&[String]>) -> Vec<MacKeyboard> {
    ids.unwrap_or(&[]).iter().map(|id| MacKeyboard { id: id.clone(), layout: true, ..Default::default() }).collect()
}

/// The keyboards a firmware has: each TextInput bundle's UIKeyboardSupportedInputModes ("fr_FR", "ja_JP-Kana",
/// "emoji"), and, from 5.x on, UIKeyboardDefaultLanguageInputModes (a language or locale's keyboards).
pub struct KeyboardCatalog {
    pub modes: BTreeMap<String, Dictionary>,
    pub defaults: BTreeMap<String, Vec<String>>,
}

impl KeyboardCatalog {
    /// The catalog of the volume's System/Library/TextInput; None where it has none (1.x).
    pub fn read(system: &Editor) -> Option<KeyboardCatalog> {
        let dir = "System/Library/TextInput";
        let bundles = system.list(dir).ok()?;
        let mut modes: BTreeMap<String, Dictionary> = BTreeMap::new();
        let mut defaults: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for b in bundles.iter().filter(|b| b.starts_with("TextInput_")) {
            let Ok(Some(info)) = plistedit::read(system, &format!("{dir}/{b}/Info.plist")) else { continue };
            // as Swift's `as? [String: [String: Any]]`: one value of another type and the whole key counts as absent
            let supported: Option<Vec<(String, Dictionary)>> = info
                .get("UIKeyboardSupportedInputModes")
                .and_then(Value::as_dictionary)
                .and_then(|d| d.iter().map(|(k, v)| v.as_dictionary().map(|m| (k.clone(), m.clone()))).collect());
            for (k, m) in supported.unwrap_or_default() {
                modes.entry(k).or_insert(m);
            }
            let languages: Option<Vec<(String, Vec<String>)>> = info
                .get("UIKeyboardDefaultLanguageInputModes")
                .and_then(Value::as_dictionary)
                .and_then(|d| d.iter().map(|(k, v)| string_array(Some(v)).map(|l| (k.clone(), l))).collect());
            for (k, l) in languages.unwrap_or_default() {
                defaults.entry(k).or_insert(l);
            }
        }
        if modes.is_empty() {
            return None;
        }
        Some(KeyboardCatalog::new(modes, defaults))
    }

    /// A catalog of these modes and defaults, one keyboard's variants folded together: 3.x's en_US-BigBear and
    /// en_US-Zephyr, both QWERTY, go by their mode's name.
    pub fn new(mut modes: BTreeMap<String, Dictionary>, defaults: BTreeMap<String, Vec<String>>) -> KeyboardCatalog {
        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for m in modes.keys().filter(|m| m.contains('-')) {
            groups.entry(base(m).to_string()).or_default().push(m.clone());
        }
        for (b, variants) in groups {
            if modes.contains_key(&b) {
                continue;
            }
            let layouts: std::collections::BTreeSet<String> = variants.iter().map(|v| layout_class(modes.get(v))).collect();
            if layouts.len() == 1 && variants.len() > 1 && !layouts.contains("") {
                let first = modes[&variants[0]].clone();
                modes.insert(b, first);
                for v in &variants {
                    modes.remove(v);
                }
            }
        }
        KeyboardCatalog { modes, defaults }
    }

    /// The mode as AppleKeyboards stores it: as the firmware's own defaults name it ("fr_FR", "ja_JP-Kana"), iOS
    /// picking its software layout, with the Mac's `hardware` layout where the mode lists it, from 5.x on
    /// ("fr_FR@hw=French", a form UIKit itself writes).
    pub fn stored(&self, mode: &str, hardware: Option<&str>) -> String {
        let listed = self.modes.get(mode).and_then(|m| string_array(m.get("HWLayouts"))).unwrap_or_default();
        let hw = hardware.and_then(|h| listed.into_iter().find(|l| squash(l) == squash(h)));
        match hw {
            Some(hw) => format!("{mode}@hw={hw}"),
            None => mode.to_string(),
        }
    }

    /// The firmware's keyboard for `language`: narrowed by the Mac layout `hardware` (7.x: fr_FR's French), a
    /// variant named in `hints` (KanaTyping: ja_JP-Kana), then `regions` in order, then name order.
    pub fn mode(&self, language: &str, hardware: Option<&str>, hints: &[&str], regions: &[String]) -> Option<String> {
        let want = script(language);
        let mut c: Vec<&String> = self.modes.keys().filter(|m| *m != "emoji" && script(base(m)) == want).collect();
        fn narrow(c: &mut Vec<&String>, keep: impl Fn(&str) -> bool) {
            if c.iter().any(|m| keep(m)) {
                c.retain(|m| keep(m));
            }
        }
        if let Some(hardware) = hardware {
            narrow(&mut c, |m| {
                let layout = self
                    .modes
                    .get(m)
                    .and_then(|d| d.get("Hardware"))
                    .and_then(Value::as_dictionary)
                    .and_then(|h| h.get("Layout"))
                    .and_then(Value::as_string);
                layout.map(squash) == Some(squash(hardware))
            });
        }
        let tokens: Vec<String> = hints
            .iter()
            .flat_map(|h| {
                h.to_lowercase()
                    .split(|ch: char| !ch.is_alphabetic() && !ch.is_numeric())
                    .filter(|t| !t.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect();
        narrow(&mut c, |m| {
            let Some(v) = m.split('-').filter(|s| !s.is_empty()).nth(1).map(str::to_lowercase) else { return false };
            tokens.iter().any(|t| t.starts_with(&v))
        });
        for r in regions {
            narrow(&mut c, |m| locale_region(base(m)).as_deref() == Some(r.as_str()));
        }
        c.first().map(|m| m.to_string())
    }

    /// AppleKeyboards for `language` and `locale`, with `extra` (create --keyboards: Mac input source IDs, of which
    /// off the Mac only Emoji & Symbols adds a keyboard).
    pub fn keyboards(&self, language: &str, locale: &str, extra: &[String]) -> Vec<String> {
        self.keyboards_mac(language, locale, &mac_keyboards(Some(extra)))
    }

    /// AppleKeyboards: the iOS keyboards for the device's `language` and `locale` first (the firmware's defaults
    /// where it lists them, else its keyboard for the language and region), then the `mac` keyboards it has, each as
    /// the firmware's keyboard for its language (in the device's region where there is a choice).
    pub fn keyboards_mac(&self, language: &str, locale: &str, mac: &[MacKeyboard]) -> Vec<String> {
        let region = locale_region(locale);
        let regions_for = |l: &str| -> Vec<String> { region.iter().cloned().chain(likely_region(l)).collect() };
        let device: Vec<String> = self
            .defaults
            .get(locale)
            .or_else(|| self.defaults.get(&language.replace('-', "_")))
            .cloned()
            .or_else(|| self.mode(language, None, &[], &regions_for(language)).map(|m| vec![m]))
            .unwrap_or_default();
        // The device's own as the firmware lists them; a Mac keyboard where its mode is not there yet, or with its
        // hardware layout in place of the bare mode.
        let mut out: Vec<String> = device.into_iter().filter(|k| self.modes.contains_key(before_at(k))).collect();
        for k in mac {
            let mut m: Option<String> = None;
            let mut hardware: Option<&str> = None;
            if k.id == EMOJI_PALETTE {
                m = self.modes.contains_key("emoji").then(|| "emoji".to_string());
            } else if let Some(l) = k.languages.first() {
                hardware = if k.layout { k.id.split('.').next_back() } else { None };
                m = self.mode(l, hardware, &[&k.id, &k.name], &regions_for(l));
            }
            let Some(m) = m else { continue };
            if let Some(i) = out.iter().position(|o| *o == m) {
                out[i] = self.stored(&m, hardware);
            } else if !out.iter().any(|o| before_at(o) == m) {
                out.push(self.stored(&m, hardware));
            }
        }
        out
    }
}

/// "fr_FR" of "fr_FR-AZERTY" or "fr_FR@hw=French".
pub fn base(mode: &str) -> &str {
    mode.split(['-', '@']).next().unwrap_or("")
}

fn before_at(k: &str) -> &str {
    k.split('@').next().unwrap_or("")
}

fn layout_class(mode: Option<&Dictionary>) -> String {
    // Swift's `as? [String: String]`: every value a string
    let class = mode.and_then(|m| m.get("UIKeyboardLayoutClass")).and_then(Value::as_dictionary);
    match class {
        Some(c) if c.values().all(|v| v.as_string().is_some()) => {
            c.get("Portrait").and_then(Value::as_string).unwrap_or("").to_string()
        }
        _ => String::new(),
    }
}

fn squash(s: &str) -> String {
    s.to_lowercase().replace(' ', "")
}

/// "zh-Hans", "ja-Jpan": a language with its script, so zh_Hans and zh_TW part.
fn script(language: &str) -> String {
    let (l, s, _) = maximize(language);
    match s {
        Some(s) if !l.is_empty() => format!("{l}-{s}"),
        Some(s) => s,
        None => l,
    }
}

/// The language's usual region ("US" for "en"), if known.
fn likely_region(language: &str) -> Vec<String> {
    maximize(language).2.into_iter().collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::plistedit::dictionary;

    fn strings(items: &[&str]) -> Value {
        plistedit::strings(items)
    }

    fn modes(pairs: Vec<(&str, Dictionary)>) -> BTreeMap<String, Dictionary> {
        pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    /// 3.1.3's shape: layout classes, two QWERTY variants of en_US.
    pub(crate) fn catalog_313() -> KeyboardCatalog {
        let layout = |c: &str| {
            dictionary([(
                "UIKeyboardLayoutClass",
                Value::Dictionary(dictionary([("Portrait", c.into()), ("Landscape", format!("{c}L").into())])),
            )])
        };
        KeyboardCatalog::new(
            modes(vec![
                ("en_GB", layout("QWERTY")),
                ("en_US-BigBear", layout("QWERTY")),
                ("en_US-Zephyr", layout("QWERTY")),
                ("fr_CA", layout("QWERTY")),
                ("fr_CH", layout("QWERTY")),
                ("fr_FR", layout("AZERTY")),
                ("ja_JP-Kana", layout("Japanese3x3")),
                ("ja_JP-Romaji", layout("QWERTY_ja_JP")),
                ("zh_Hans-HWR", layout("HWR")),
                ("zh_Hans-Pinyin", layout("Pinyin")),
                ("zh_Hant-HWR", layout("HWR")),
                ("emoji", layout("Emoji")),
            ]),
            BTreeMap::new(),
        )
    }

    /// 7.1.2's shape: hardware and software layouts, and the language defaults.
    pub(crate) fn catalog_712() -> KeyboardCatalog {
        let hwsw = |sw: &[&str], hw: &[&str], hardware: Option<&str>| {
            let mut m = dictionary([("SWLayouts", strings(sw))]);
            if !hw.is_empty() {
                m.insert("HWLayouts".into(), strings(hw));
            }
            if let Some(h) = hardware {
                m.insert("Hardware".into(), Value::Dictionary(dictionary([("Layout", h.into()), ("USBCountry", 8.into())])));
            }
            m
        };
        let defaults: BTreeMap<String, Vec<String>> = [
            ("en", vec!["en_US"]),
            ("en_GB", vec!["en_GB"]),
            ("fr_FR", vec!["fr_FR"]),
            ("ja", vec!["ja_JP-Kana", "ja_JP-Romaji", "emoji", "en_US"]),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.into_iter().map(str::to_string).collect()))
        .collect();
        KeyboardCatalog::new(
            modes(vec![
                ("en_US", hwsw(&["QWERTY", "AZERTY"], &["US", "British", "French"], Some("US"))),
                ("en_GB", hwsw(&["QWERTY"], &["British", "US"], Some("British"))),
                ("fr_FR", hwsw(&["AZERTY-French", "AZERTY"], &["French", "French - numerical", "US"], Some("French"))),
                ("fr_CA", hwsw(&["QWERTY-Accents"], &["Canadian - CSA", "French", "US"], Some("Canadian - CSA"))),
                ("ja_JP", hwsw(&["Kana", "Kana-Flick"], &[], None)),
                ("ja_JP-Kana", hwsw(&["Kana"], &["US", "KANA"], None)),
                ("ja_JP-Romaji", hwsw(&["QWERTY-Japanese"], &["US", "French"], Some("US"))),
                ("zh_Hans-HWR", hwsw(&["HWR"], &[], None)),
                ("zh_Hans-Pinyin", hwsw(&["Pinyin"], &["US"], Some("US"))),
                ("emoji", hwsw(&["Emoji"], &[], None)),
            ]),
            defaults,
        )
    }

    fn k(id: &str, languages: &[&str], name: &str, layout: bool) -> MacKeyboard {
        MacKeyboard {
            id: id.into(),
            languages: languages.iter().map(|s| s.to_string()).collect(),
            name: name.into(),
            layout,
        }
    }

    /// keyboardsAreTheLanguagesThenTheMacsTheFirmwareHas: the device language's keyboard first, then the Mac's
    /// keyboards as the firmware names them, named as its own defaults name them, and nothing it lacks.
    #[test]
    fn keyboards_are_the_languages_then_the_macs_the_firmware_has() {
        let v313 = catalog_313();
        let v712 = catalog_712();
        assert!(v313.modes.contains_key("en_US") && !v313.modes.contains_key("en_US-Zephyr"), "variants folded");
        let french = k("com.apple.keylayout.French", &["fr", "acf"], "", true);
        let swiss_french = k("com.apple.keylayout.SwissFrench", &["fr"], "", true);
        let us = k("com.apple.keylayout.US", &["en", "fr"], "", true);
        let british = k("com.apple.keylayout.British", &["en"], "", true);
        let kana = k("com.apple.inputmethod.Kotoeri.KanaTyping.Japanese", &["ja"], "Hiragana", false);
        let romaji = k("com.apple.inputmethod.Kotoeri.RomajiTyping.Japanese.Katakana", &["ja"], "", false);
        let pinyin = k("com.apple.inputmethod.SCIM.ITABC", &["zh-Hans"], "Pinyin - Simplified", false);
        let korean = k("com.apple.inputmethod.Korean.2SetKorean", &["ko"], "2-Set Korean", false);
        let emoji = k(EMOJI_PALETTE, &["en"], "", false);
        let kb = |c: &KeyboardCatalog, l: &str, loc: &str, mac: &[MacKeyboard]| c.keyboards_mac(l, loc, mac);

        assert_eq!(
            kb(&v313, "fr", "fr_FR", &[french.clone(), kana.clone(), us.clone(), korean.clone(), pinyin.clone(), emoji.clone()]),
            ["fr_FR", "ja_JP-Kana", "en_US", "zh_Hans-Pinyin", "emoji"]
        );
        assert_eq!(kb(&v313, "ja", "ja_JP", &[romaji, us.clone()]), ["ja_JP-Kana", "ja_JP-Romaji", "en_US"]);
        assert_eq!(kb(&v313, "en", "en_US", &[]), ["en_US"], "the built-in iPod: English only");
        assert_eq!(kb(&v313, "fr", "fr_CA", &[]), ["fr_CA"]);
        // A region format in another language than the device's (3.2's English with uk_UA): the language's keyboard.
        assert_eq!(kb(&v313, "en", "uk_UA", &[]), ["en_US"]);
        assert_eq!(kb(&v712, "en", "uk_UA", &[us.clone()]), ["en_US@hw=US"]);
        assert_eq!(kb(&v712, "fr", "fr_FR", &[french.clone(), kana, us.clone()]), ["fr_FR@hw=French", "ja_JP-Kana", "en_US@hw=US"]);
        assert_eq!(
            kb(&v712, "en", "en_US", &[british, us, pinyin, emoji, korean]),
            ["en_US@hw=US", "en_GB@hw=British", "zh_Hans-Pinyin", "emoji"]
        );
        assert_eq!(kb(&v712, "ja", "ja_JP", &[french]), ["ja_JP-Kana", "ja_JP-Romaji", "emoji", "en_US", "fr_FR@hw=French"]);
        // A Mac layout the keyboard does not list as a hardware layout: the bare keyboard, its own.
        assert_eq!(kb(&v712, "en", "en_US", &[swiss_french]), ["en_US", "fr_FR"]);
    }

    /// Off the Mac, `create --keyboards` IDs carry no languages: only Emoji & Symbols adds a keyboard.
    #[test]
    fn off_the_mac_only_the_emoji_palette_adds_a_keyboard() {
        let v712 = catalog_712();
        let ids = vec!["com.apple.keylayout.French".to_string(), EMOJI_PALETTE.to_string()];
        assert_eq!(v712.keyboards("en", "en_US", &ids), ["en_US", "emoji"]);
        assert_eq!(v712.keyboards("en", "en_US", &[]), ["en_US"]);
    }
}
