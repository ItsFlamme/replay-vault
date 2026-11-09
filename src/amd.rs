//! AMD Software (Adrenalin): its Instant Replay records alongside ours, and its hotkeys (Alt+Z,
//! Ctrl+Shift+S...) can be the same as ours, so both apps would react. Only read, never changed.

use std::collections::BTreeMap;

use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, REG_DWORD, REG_SZ, RegCloseKey, RegEnumValueW, RegOpenKeyExW,
};
use windows::core::{PWSTR, w};

use crate::hotkey::Combo;

/// AMD Software's background programs.
const EXES: [&str; 2] = ["AMDRSServ.exe", "RadeonSoftware.exe"];

/// A value under Software\AMD\DVR.
#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Dword(u32),
    Text(String),
}

/// What AMD Software is set to, as far as clashes go.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Amd {
    /// Its Instant Replay is recording (DvrEnabled and InstantReplayEnabled both 1).
    pub replay_on: bool,
    /// HotkeysDisabled is 1: none of its keys fire.
    pub hotkeys_off: bool,
    /// Each *Hotkey value's name (without "Hotkey") and its keys.
    pub hotkeys: Vec<(String, Combo)>,
}

/// AMD's "Mods,Key" (like "Alt,Z", "Ctrl+Shift,S" or "Ctrl+Alt,VK_UP") as a combo; None if
/// empty or not a key (like "MOUSE_MIDDLE").
pub fn parse_hotkey(s: &str) -> Option<Combo> {
    let parts: Vec<&str> = s.split([',', '+']).map(|p| p.trim().trim_start_matches("VK_")).collect();
    Combo::parse(&parts.join("+"))
}

/// The machine-wide values with the user's own on top.
pub fn merge(hklm: BTreeMap<String, Val>, hkcu: BTreeMap<String, Val>) -> BTreeMap<String, Val> {
    let mut all = hklm;
    all.extend(hkcu);
    all
}

/// Hotkey names (any case) that belong to Record & Stream; they do nothing while DvrEnabled is off.
const DVR_WORDS: [&str; 8] = ["dvr", "replay", "record", "gif", "stream", "camera", "mic", "screenshot"];

impl Amd {
    pub fn from_values(values: &BTreeMap<String, Val>) -> Amd {
        let on = |name: &str| values.get(name) == Some(&Val::Dword(1));
        let dvr_on = on("DvrEnabled");
        let hotkeys = values
            .iter()
            .filter_map(|(name, v)| match v {
                Val::Text(s) => Some((name.strip_suffix("Hotkey")?.to_string(), parse_hotkey(s)?)),
                Val::Dword(_) => None,
            })
            .filter(|(name, _)| dvr_on || !DVR_WORDS.iter().any(|w| name.to_ascii_lowercase().contains(w)))
            .collect();
        Amd { replay_on: on("DvrEnabled") && on("InstantReplayEnabled"), hotkeys_off: on("HotkeysDisabled"), hotkeys }
    }

    /// Our hotkeys (name, keys) that AMD also uses, as (keys, our name).
    pub fn clashes<'a>(&self, ours: &[(&'a str, Combo)]) -> Vec<(Combo, &'a str)> {
        if self.hotkeys_off {
            return Vec::new();
        }
        ours.iter().filter(|(_, c)| self.hotkeys.iter().any(|(_, a)| a == c)).map(|(name, c)| (*c, *name)).collect()
    }
}

/// Every DWORD and string value of `root`\Software\AMD\DVR; None if the key isn't there.
fn read_key(root: HKEY) -> Option<BTreeMap<String, Val>> {
    let mut key = HKEY::default();
    if unsafe { RegOpenKeyExW(root, w!(r"Software\AMD\DVR"), None, KEY_READ, &mut key) }.is_err() {
        return None;
    }
    let mut out = BTreeMap::new();
    for i in 0.. {
        let mut name = [0u16; 256];
        let mut name_len = name.len() as u32;
        let mut kind = 0u32;
        let mut data = [0u8; 1024];
        let mut data_len = data.len() as u32;
        let r = unsafe {
            RegEnumValueW(key, i, Some(PWSTR(name.as_mut_ptr())), &mut name_len, None, Some(&mut kind), Some(data.as_mut_ptr()), Some(&mut data_len))
        };
        if r.0 == 259 {
            break; // ERROR_NO_MORE_ITEMS
        }
        if r.is_err() {
            continue; // too long for the buffers; not one of the values we look at
        }
        let name = String::from_utf16_lossy(&name[..name_len as usize]);
        let data = &data[..data_len as usize];
        if kind == REG_DWORD.0 && data.len() == 4 {
            out.insert(name, Val::Dword(u32::from_le_bytes([data[0], data[1], data[2], data[3]])));
        } else if kind == REG_SZ.0 {
            let wide: Vec<u16> = data.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).take_while(|&c| c != 0).collect();
            out.insert(name, Val::Text(String::from_utf16_lossy(&wide)));
        }
    }
    let _ = unsafe { RegCloseKey(key) };
    Some(out)
}

/// AMD Software's settings, or None if it isn't on this PC (no DVR key and none of its programs
/// running). Not tied to the GPU that encodes: a PC can have AMD Software for a second GPU.
pub fn detect() -> Option<Amd> {
    let (hklm, hkcu) = (read_key(HKEY_LOCAL_MACHINE), read_key(HKEY_CURRENT_USER));
    let running = crate::tweaks::programs().iter().any(|(_, _, n)| EXES.iter().any(|e| n.eq_ignore_ascii_case(e)));
    if hklm.is_none() && hkcu.is_none() && !running {
        return None;
    }
    Some(Amd::from_values(&merge(hklm.unwrap_or_default(), hkcu.unwrap_or_default())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vals(list: &[(&str, Val)]) -> BTreeMap<String, Val> {
        list.iter().map(|(n, v)| (n.to_string(), v.clone())).collect()
    }

    #[test]
    fn parses_amd_keys() {
        assert_eq!(parse_hotkey("Alt,Z"), Combo::parse("Alt+Z"));
        assert_eq!(parse_hotkey("Ctrl,Shift,S").unwrap().to_string(), "Ctrl+Shift+S");
        // As a real AMD Software install writes them.
        assert_eq!(parse_hotkey("Ctrl+Shift,S").unwrap().to_string(), "Ctrl+Shift+S");
        assert_eq!(parse_hotkey("Ctrl+Alt,VK_UP").unwrap().to_string(), "Ctrl+Alt+Up");
        assert_eq!(parse_hotkey("MOUSE_MIDDLE"), None);
        assert_eq!(parse_hotkey("F9").unwrap().to_string(), "F9");
        assert_eq!(parse_hotkey(""), None);
        assert_eq!(parse_hotkey("Alt,"), None);
        assert_eq!(parse_hotkey("S"), None, "a bare letter isn't a hotkey");
    }

    #[test]
    fn user_values_win() {
        let hklm = vals(&[("DvrEnabled", Val::Dword(1)), ("InstantReplayEnabled", Val::Dword(1)), ("ToggleRsHotkey", Val::Text("Alt,R".into()))]);
        let hkcu = vals(&[("InstantReplayEnabled", Val::Dword(0)), ("ToggleRsHotkey", Val::Text("Alt,X".into()))]);
        let all = merge(hklm.clone(), hkcu);
        assert_eq!(all["DvrEnabled"], Val::Dword(1));
        assert_eq!(all["InstantReplayEnabled"], Val::Dword(0));
        let amd = Amd::from_values(&all);
        assert!(!amd.replay_on);
        assert_eq!(amd.hotkeys, [("ToggleRs".to_string(), Combo::parse("Alt+X").unwrap())]);
        assert!(Amd::from_values(&hklm).replay_on);
    }

    #[test]
    fn clash_rules() {
        let ours = [("Save clip", Combo::parse("Alt+F10").unwrap()), ("Show / hide this window", Combo::parse("Alt+Z").unwrap())];
        let base = vals(&[
            ("DvrEnabled", Val::Dword(1)),
            ("InstantReplayEnabled", Val::Dword(0)),
            ("ToggleDvrToolbarHotkey", Val::Text("Alt,Z".into())),
            ("SaveInstantReplayHotkey", Val::Text("Ctrl,Shift,S".into())),
            ("SomethingElse", Val::Text("Alt,F10".into())), // not a *Hotkey value
        ]);
        let amd = Amd::from_values(&base);
        assert!(!amd.replay_on, "Instant Replay needs both switches on");
        assert_eq!(amd.clashes(&ours), [(Combo::parse("Alt+Z").unwrap(), "Show / hide this window")]);

        let mut off = base.clone();
        off.insert("HotkeysDisabled".into(), Val::Dword(1));
        assert!(Amd::from_values(&off).clashes(&ours).is_empty(), "AMD's keys are switched off");

        let mut dvr_off = base.clone();
        dvr_off.insert("DvrEnabled".into(), Val::Dword(0));
        dvr_off.insert("ToggleRsHotkey".into(), Val::Text("Alt,F10".into()));
        assert_eq!(
            Amd::from_values(&dvr_off).clashes(&ours),
            [(Combo::parse("Alt+F10").unwrap(), "Save clip")],
            "with recording off only non-recording keys like opening AMD Software count"
        );

        let mut replay = base;
        replay.insert("InstantReplayEnabled".into(), Val::Dword(1));
        assert!(Amd::from_values(&replay).replay_on);
        assert!(Amd::default().clashes(&ours).is_empty());
    }
}
