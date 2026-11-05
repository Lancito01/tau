//! Settings file, launch at startup, and the Send To shortcut.
use crate::form::{self, Field};
use crate::gfx::wz;
use std::path::PathBuf;
use windows::core::*;
use windows::Win32::System::Com::*;
use windows::Win32::System::Registry::*;
use windows::Win32::UI::Shell::*;

const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
const RUN_VALUE: PCWSTR = w!("Tau");

#[derive(Clone, PartialEq, Debug)]
pub struct Settings {
    /// Off: Win opens the wheel and scrolling switches wheels. On: each wheel has its own key below.
    pub separate_keys: bool,
    pub convert_key: String,
    pub tools_key: String,
    pub actions_key: String,
    pub hotkey: String,
    pub auto_update: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings { separate_keys: false, convert_key: "Shift".into(), tools_key: "Ctrl".into(), actions_key: "Alt".into(), hotkey: "Ctrl+Alt+Space".into(), auto_update: true }
    }
}

pub const KEYS: [&str; 7] = ["Shift", "Ctrl", "Alt", "Win", "Caps Lock", "Tab", "Off"];
pub const HOTKEYS: [&str; 4] = ["Ctrl+Alt+Space", "Ctrl+Shift+Space", "Win+Alt+Space", "Off"];

fn dir() -> PathBuf {
    let d = PathBuf::from(std::env::var("APPDATA").unwrap_or_else(|_| ".".into())).join("Tau");
    let _ = std::fs::create_dir_all(&d);
    d
}

impl Settings {
    pub fn parse(s: &str) -> Settings {
        let mut c = Settings::default();
        for l in s.lines() {
            let Some((k, v)) = l.split_once('=') else { continue };
            let v = v.trim().to_string();
            match k.trim() {
                "separate_keys" => c.separate_keys = v == "1",
                "convert_key" if KEYS.contains(&v.as_str()) => c.convert_key = v,
                "tools_key" if KEYS.contains(&v.as_str()) => c.tools_key = v,
                "actions_key" if KEYS.contains(&v.as_str()) => c.actions_key = v,
                "hotkey" if HOTKEYS.contains(&v.as_str()) => c.hotkey = v,
                "auto_update" => c.auto_update = v == "1",
                _ => {}
            }
        }
        c
    }
    pub fn serialize(&self) -> String {
        format!("separate_keys={}\nconvert_key={}\ntools_key={}\nactions_key={}\nhotkey={}\nauto_update={}\n", self.separate_keys as u8, self.convert_key, self.tools_key, self.actions_key, self.hotkey, self.auto_update as u8)
    }
    pub fn load() -> Settings { std::fs::read_to_string(dir().join("settings.ini")).map(|s| Self::parse(&s)).unwrap_or_default() }
    pub fn save(&self) { let _ = std::fs::write(dir().join("settings.ini"), self.serialize()); }
}

pub fn exe() -> String { std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default() }

/// The registry is the source of truth, so the toggle can never disagree with Windows.
pub fn startup_enabled() -> bool {
    unsafe {
        let mut size = 0u32;
        RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, None, None, Some(&mut size)).is_ok()
    }
}

pub fn set_startup(on: bool) {
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, KEY_SET_VALUE, &mut key).is_err() { return; }
        if on {
            let v = wz(&format!("\"{}\"", exe()));
            let bytes = std::slice::from_raw_parts(v.as_ptr() as *const u8, v.len() * 2);
            let _ = RegSetValueExW(key, RUN_VALUE, None, REG_SZ, Some(bytes));
        } else {
            let _ = RegDeleteValueW(key, RUN_VALUE);
        }
        let _ = RegCloseKey(key);
    }
}

fn sendto_link() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var("APPDATA").ok()?).join(r"Microsoft\Windows\SendTo\Tau.lnk"))
}

pub fn sendto_enabled() -> bool { sendto_link().is_some_and(|p| p.exists()) }

pub fn set_sendto(on: bool) {
    let Some(link) = sendto_link() else { return };
    if !on { let _ = std::fs::remove_file(link); return; }
    unsafe {
        let Ok(sl): Result<IShellLinkW> = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER) else { return };
        let target = HSTRING::from(exe());
        let _ = sl.SetPath(&target);
        let _ = sl.SetArguments(w!("--wheel"));
        if let Ok(pf) = sl.cast::<IPersistFile>() { let _ = pf.Save(&HSTRING::from(link.as_os_str()), true); }
    }
}

/// Carries settings, the startup entry and the Send To link over from when this app was called Orrery.
pub fn migrate() {
    const OLD: &str = "Orrery";
    let Ok(appdata) = std::env::var("APPDATA") else { return };
    let appdata = PathBuf::from(appdata);
    let (old_ini, new_ini) = (appdata.join(OLD).join("settings.ini"), dir().join("settings.ini"));
    if old_ini.exists() && !new_ini.exists() { let _ = std::fs::copy(&old_ini, &new_ini); }
    let old_link = appdata.join(r"Microsoft\Windows\SendTo").join(format!("{OLD}.lnk"));
    if old_link.exists() {
        let _ = std::fs::remove_file(&old_link);
        set_sendto(true);
    }
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, KEY_SET_VALUE | KEY_QUERY_VALUE, &mut key).is_err() { return; }
        let name = HSTRING::from(OLD);
        let mut size = 0u32;
        if RegQueryValueExW(key, &name, None, None, None, Some(&mut size)).is_ok() {
            let _ = RegDeleteValueW(key, &name);
            set_startup(true);
        }
        let _ = RegCloseKey(key);
    }
}

fn fields(cur: &Settings, startup: bool, sendto: bool) -> Vec<Field> {
    let idx = |list: &[&str], v: &str| list.iter().position(|k| *k == v).unwrap_or(0);
    let keys: Vec<String> = KEYS.iter().map(|s| s.to_string()).collect();
    let e = crate::engines();
    let mut about = format!("Version {}.", crate::VERSION);
    if !e.ffmpeg { about.push_str(" Put ffmpeg.exe on your PATH or next to Tau.exe to turn on audio and video."); }
    vec![
        Field::Heading("Keys"),
        Field::Note("During a drag, press Win to open the dial, then scroll to switch wheels. Trackpads can't scroll mid-drag, so turn this on to give each wheel its own key.".into()),
        Field::Check("Use a separate key for each wheel", cur.separate_keys),
        Field::ChoiceIf(2, "Convert wheel", keys.clone(), idx(&KEYS, &cur.convert_key)),
        Field::ChoiceIf(2, "Tools wheel", keys.clone(), idx(&KEYS, &cur.tools_key)),
        Field::ChoiceIf(2, "Actions wheel", keys, idx(&KEYS, &cur.actions_key)),
        form::choice("Keyboard shortcut", &HOTKEYS, idx(&HOTKEYS, &cur.hotkey)),
        Field::Heading("General"),
        Field::Check("Start Tau when I sign in to Windows", startup),
        Field::Check("Add Tau to the Send To menu", sendto),
        Field::Check("Install updates automatically", cur.auto_update),
        Field::Heading("Engines"),
        Field::Status(vec![("ffmpeg".into(), e.ffmpeg), ("Word".into(), e.word), ("Excel".into(), e.excel), ("PowerPoint".into(), e.powerpoint)]),
        Field::Note(about),
    ]
}

/// Shows the settings form. Returns the new settings if the user saved.
pub fn show(cur: &Settings) -> Option<Settings> {
    form::set_context(String::new());
    let v = form::show("Settings", &fields(cur, startup_enabled(), sendto_enabled()), "Save")?;
    let s = Settings { separate_keys: v[2] == "1", convert_key: v[3].clone(), tools_key: v[4].clone(), actions_key: v[5].clone(), hotkey: v[6].clone(), auto_update: v[10] == "1" };
    if v[8] == "1" { set_startup(true); } else if startup_enabled() { set_startup(false); }
    if v[9] == "1" { set_sendto(true); } else if sendto_enabled() { set_sendto(false); }
    s.save();
    Some(s)
}

/// The settings form as a PNG, for design review.
pub fn render(out: &std::path::Path) -> std::result::Result<(), String> {
    form::render("Settings", "", &fields(&Settings::default(), true, false), "Save", out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip() {
        let s = Settings { separate_keys: true, convert_key: "Tab".into(), auto_update: false, ..Default::default() };
        assert_eq!(Settings::parse(&s.serialize()), s);
    }
    #[test]
    fn older_settings_files_get_win_and_scroll() {
        let s = Settings::parse("convert_key=Tab\ntools_key=Ctrl\nactions_key=Alt\n");
        assert!(!s.separate_keys);
        assert_eq!(s.convert_key, "Tab");
    }
    #[test]
    fn junk_falls_back_to_defaults() {
        assert_eq!(Settings::parse("convert_key=Banana\nnonsense\n"), Settings::default());
    }
}

#[cfg(test)]
mod startup_tests {
    use super::*;

    // Touches the real Run key, so it only runs when asked for: cargo test startup_round_trip -- --ignored
    #[test]
    #[ignore]
    fn startup_round_trip() {
        assert!(!startup_enabled(), "a Tau startup entry already exists; not touching it");
        set_startup(true);
        let on = startup_enabled();
        let mut size = 0u32;
        unsafe { let _ = RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, RUN_VALUE, RRF_RT_REG_SZ, None, None, Some(&mut size)); }
        set_startup(false);
        assert!(on, "toggle on did not write the Run value");
        assert!(size as usize > exe().len(), "the Run value is shorter than the quoted exe path");
        assert!(!startup_enabled(), "toggle off did not remove the Run value");
    }
}
