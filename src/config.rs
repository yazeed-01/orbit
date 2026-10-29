//! Persistent settings (a tiny key=value file) and the autostart registry entry.

use std::path::PathBuf;

use windows::Win32::UI::Input::KeyboardAndMouse::*;

pub struct Hotkey {
    pub label: &'static str,
    pub mods: HOT_KEY_MODIFIERS,
    pub vk: u32,
}

pub const HOTKEYS: &[Hotkey] = &[
    Hotkey { label: "Ctrl+Alt+Space", mods: HOT_KEY_MODIFIERS(MOD_CONTROL.0 | MOD_ALT.0), vk: VK_SPACE.0 as u32 },
    Hotkey { label: "Ctrl+Shift+Space", mods: HOT_KEY_MODIFIERS(MOD_CONTROL.0 | MOD_SHIFT.0), vk: VK_SPACE.0 as u32 },
    Hotkey { label: "Ctrl+Alt+R", mods: HOT_KEY_MODIFIERS(MOD_CONTROL.0 | MOD_ALT.0), vk: 'R' as u32 },
    Hotkey { label: "Win+Alt+Z", mods: HOT_KEY_MODIFIERS(MOD_WIN.0 | MOD_ALT.0), vk: 'Z' as u32 },
    Hotkey { label: "Alt+`", mods: MOD_ALT, vk: VK_OEM_3.0 as u32 },
];

pub struct Config {
    pub hotkey: usize,
    pub enabled: bool,
    pub first_run: bool,
}

fn path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let d = base.join("Orbit");
    let _ = std::fs::create_dir_all(&d);
    d.join("config.ini")
}

impl Config {
    pub fn load() -> Self {
        let mut c = Config { hotkey: 0, enabled: false, first_run: true };
        if let Ok(text) = std::fs::read_to_string(path()) {
            c.first_run = false;
            for line in text.lines() {
                if let Some((k, v)) = line.split_once('=') {
                    match k.trim() {
                        "hotkey" => c.hotkey = v.trim().parse().unwrap_or(0).min(HOTKEYS.len() - 1),
                        "enabled" => c.enabled = v.trim() == "1",
                        _ => {}
                    }
                }
            }
        }
        c
    }

    pub fn save(&self) {
        let _ = std::fs::write(path(), format!("hotkey={}\nenabled={}\n", self.hotkey, self.enabled as u8));
    }
}

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

pub fn autostart() -> bool {
    use winreg::enums::*;
    winreg::RegKey::predef(HKEY_CURRENT_USER)
        .open_subkey(RUN_KEY)
        .and_then(|k| k.get_value::<String, _>("Orbit"))
        .is_ok()
}

pub fn set_autostart(on: bool) {
    use winreg::enums::*;
    let Ok((key, _)) = winreg::RegKey::predef(HKEY_CURRENT_USER).create_subkey(RUN_KEY) else { return };
    if on {
        if let Ok(exe) = std::env::current_exe() {
            let _ = key.set_value("Orbit", &format!("\"{}\" --background", exe.display()));
        }
    } else {
        let _ = key.delete_value("Orbit");
    }
}

fn recent_path() -> PathBuf {
    path().with_file_name("recent.txt")
}

/// Recent Move/Copy destinations (newest first), topped up with common folders.
pub fn recent_folders() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_to_string(recent_path())
        .unwrap_or_default()
        .lines()
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .collect();
    if let Some(home) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
        for d in ["Desktop", "Downloads", "Documents", "Pictures"] {
            let p = home.join(d);
            if p.is_dir() && !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

pub fn push_recent(dir: &std::path::Path) {
    let mut list: Vec<PathBuf> = std::fs::read_to_string(recent_path()).unwrap_or_default().lines().map(PathBuf::from).collect();
    list.retain(|p| p != dir);
    list.insert(0, dir.to_path_buf());
    list.truncate(8);
    let text = list.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n");
    let _ = std::fs::write(recent_path(), text);
}

pub fn downloads_dir() -> PathBuf {
    std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join("Downloads")).filter(|p| p.is_dir()).unwrap_or_else(std::env::temp_dir)
}
