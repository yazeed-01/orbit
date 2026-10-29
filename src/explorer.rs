//! Finding out what the user right-clicked in Explorer / on the Desktop.

use std::path::PathBuf;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::System::Com::*;
use windows::Win32::UI::Accessibility::*;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::ui;

/// Everything the menu and actions need to know about the click.
pub struct Ctx {
    pub folder: PathBuf,
    pub items: Vec<PathBuf>,
    pub background: bool,
    pub pt: POINT,
    pub view: Option<IShellView>,
    pub defview: HWND,
}

const SID_STOP_LEVEL_BROWSER: GUID = GUID::from_u128(0x4C96BE40_915C_11CF_99D3_00AA004AE837);

/// Cheap check used inside the mouse hook: is this point over an Explorer file view
/// (or the Desktop)? Returns the SHELLDLL_DefView window if so.
pub fn defview_at(p: POINT) -> Option<HWND> {
    unsafe {
        let mut h = WindowFromPoint(p);
        let mut depth = 0;
        while !h.is_invalid() && depth < 12 {
            if ui::class_name(h) == "SHELLDLL_DefView" {
                let root = GetAncestor(h, GA_ROOT);
                let rc = ui::class_name(root);
                if rc == "CabinetWClass" || rc == "ExploreWClass" || rc == "Progman" || rc == "WorkerW" {
                    return Some(h);
                }
                return None;
            }
            h = GetParent(h).unwrap_or_default();
            depth += 1;
        }
        None
    }
}

/// Use UI Automation to see whether the click landed on an item; if that item is not
/// already part of the selection, select it (mimicking normal right-click behaviour).
/// Returns true if the click was on an item.
fn prepare_selection(p: POINT) -> bool {
    unsafe {
        let Ok(uia) = CoCreateInstance::<_, IUIAutomation>(&CUIAutomation, None, CLSCTX_INPROC_SERVER) else { return false };
        let Ok(mut el) = uia.ElementFromPoint(p) else { return false };
        let Ok(walker) = uia.ControlViewWalker() else { return false };
        for _ in 0..4 {
            let ct = el.CurrentControlType().unwrap_or_default();
            if ct == UIA_ListItemControlTypeId {
                if let Ok(sel) = el.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(UIA_SelectionItemPatternId) {
                    let selected = sel.CurrentIsSelected().map(|b| b.as_bool()).unwrap_or(false);
                    if !selected {
                        let _ = sel.Select();
                    }
                }
                return true;
            }
            if ct == UIA_ListControlTypeId || ct == UIA_PaneControlTypeId || ct == UIA_WindowControlTypeId {
                return false;
            }
            match walker.GetParentElement(&el) {
                Ok(parent) => el = parent,
                Err(_) => return false,
            }
        }
        false
    }
}

fn shell_browser_for(defview: HWND) -> Option<IShellBrowser> {
    unsafe {
        let windows: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_ALL).ok()?;
        let root = GetAncestor(defview, GA_ROOT);
        let rc = ui::class_name(root);
        if rc == "Progman" || rc == "WorkerW" {
            let loc = VARIANT::from(0i32); // CSIDL_DESKTOP
            let empty = VARIANT::default();
            let mut hwnd = 0i32;
            let disp = windows.FindWindowSW(&loc, &empty, SWC_DESKTOP, &mut hwnd, SWFO_NEEDDISPATCH).ok()?;
            let sp: IServiceProvider = disp.cast().ok()?;
            return sp.QueryService(&SID_STOP_LEVEL_BROWSER).ok();
        }
        // The browser's window is the tab host (Windows 11) or the frame (Windows 10);
        // either way it is an ancestor of the DefView we clicked.
        let mut ancestors = Vec::new();
        let mut h = defview;
        while !h.is_invalid() {
            ancestors.push(h);
            h = GetParent(h).unwrap_or_default();
        }
        let count = windows.Count().ok()?;
        for i in 0..count {
            let Ok(disp) = windows.Item(&VARIANT::from(i)) else { continue };
            let Ok(sp) = disp.cast::<IServiceProvider>() else { continue };
            let Ok(sb) = sp.QueryService::<IShellBrowser>(&SID_STOP_LEVEL_BROWSER) else { continue };
            let Ok(sv) = sb.QueryActiveShellView() else { continue };
            if let Ok(w) = sv.GetWindow() {
                if w == defview {
                    return Some(sb);
                }
            }
            if let Ok(w) = sb.GetWindow() {
                if w != root && ancestors.contains(&w) {
                    return Some(sb);
                }
            }
        }
        None
    }
}

fn item_path(item: &IShellItem) -> Option<PathBuf> {
    unsafe {
        let p = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as _));
        s.map(PathBuf::from)
    }
}

/// Build the click context. None means "not a file-system location" (e.g. This PC),
/// in which case the caller falls back to the classic menu.
pub fn context_at(p: POINT, defview: HWND) -> Option<Ctx> {
    let on_item = prepare_selection(p);
    context_for(p, defview, on_item)
}

/// Context for a known view; `on_item` = the click was on a (now selected) item.
pub fn context_for(p: POINT, defview: HWND, on_item: bool) -> Option<Ctx> {
    unsafe {
        let sb = shell_browser_for(defview)?;
        let view = sb.QueryActiveShellView().ok()?;
        let fv: IFolderView2 = view.cast().ok()?;
        let folder_item: IShellItem = fv.GetFolder().ok()?;
        let folder = item_path(&folder_item)?;
        let mut items = Vec::new();
        if on_item {
            if let Ok(arr) = fv.GetSelection(false) {
                let n = arr.GetCount().unwrap_or(0);
                for i in 0..n {
                    if let Ok(it) = arr.GetItemAt(i) {
                        if let Some(path) = item_path(&it) {
                            items.push(path);
                        }
                    }
                }
            }
            if items.is_empty() {
                return None; // virtual items (e.g. drives in This PC, recycle bin)
            }
        }
        Some(Ctx { folder, background: items.is_empty(), items, pt: p, view: Some(view), defview })
    }
}

/// Select a newly created item in the view and start inline rename.
pub fn select_and_rename(view: &IShellView, path: &std::path::Path) {
    unsafe {
        let w = ui::wide(&path.to_string_lossy());
        let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
        if SHParseDisplayName(PCWSTR(w.as_ptr()), None, &mut pidl, 0, None).is_ok() && !pidl.is_null() {
            let child = ILFindLastID(pidl);
            let flags = SVSI_SELECT.0 | SVSI_DESELECTOTHERS.0 | SVSI_ENSUREVISIBLE.0 | SVSI_FOCUSED.0 | SVSI_EDIT.0;
            let _ = view.SelectItem(child, flags as u32);
            CoTaskMemFree(Some(pidl as _));
        }
    }
}

/// Ask an Explorer view to refresh (F5 equivalent).
pub fn refresh(defview: HWND) {
    unsafe {
        let _ = PostMessageW(defview, WM_COMMAND, WPARAM(41504), LPARAM(0));
    }
}

/// Find the file view inside an Explorer (or Desktop) top-level window.
pub fn find_defview(top: HWND) -> Option<HWND> {
    unsafe extern "system" fn cb(h: HWND, lp: LPARAM) -> BOOL {
        let out = &mut *(lp.0 as *mut HWND);
        if ui::class_name(h) == "SHELLDLL_DefView" && IsWindowVisible(h).as_bool() {
            *out = h;
            return FALSE;
        }
        TRUE
    }
    let mut found = HWND::default();
    unsafe {
        let _ = EnumChildWindows(top, Some(cb), LPARAM(&mut found as *mut HWND as isize));
    }
    (!found.is_invalid()).then_some(found)
}
