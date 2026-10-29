//! Copy / move / delete through the Windows shell, so users get the native progress
//! window, conflict dialogs, Recycle Bin and Ctrl+Z undo in Explorer.

use std::path::{Path, PathBuf};

use windows::core::*;
use windows::Win32::System::Com::*;
use windows::Win32::UI::Shell::*;

use crate::hud::ToastKind;
use crate::jobs;

pub enum Op {
    Copy,
    Move,
    /// true = permanent (skip the Recycle Bin, Windows asks to confirm)
    Delete(bool),
}

/// Runs on its own thread; `done` is toasted on success (empty = silent).
pub fn run(op: Op, items: Vec<PathBuf>, dest: Option<PathBuf>, done: String) {
    if items.is_empty() {
        return;
    }
    std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        match perform(&op, &items, dest.as_deref()) {
            Ok(false) if !done.is_empty() => jobs::toast(done, ToastKind::Ok),
            Ok(_) => {}
            Err(e) => jobs::toast(format!("File operation failed: {}", e.message()), ToastKind::Error),
        }
        CoUninitialize();
    });
}

/// Ok(true) when the user cancelled part of it.
unsafe fn perform(op: &Op, items: &[PathBuf], dest: Option<&Path>) -> Result<bool> {
    let fo: IFileOperation = CoCreateInstance(&FileOperation, None, CLSCTX_ALL)?;
    let mut flags = FOF_ALLOWUNDO.0 as u32 | FOF_NOCONFIRMMKDIR.0 as u32;
    match op {
        Op::Delete(true) => flags &= !(FOF_ALLOWUNDO.0 as u32),
        Op::Delete(false) => flags |= FOFX_RECYCLEONDELETE.0 as u32,
        Op::Copy => {
            // pasting into the same folder makes "name - Copy" like Explorer does
            if items.iter().any(|p| p.parent() == dest) {
                flags |= FOF_RENAMEONCOLLISION.0 as u32;
            }
        }
        Op::Move => {}
    }
    fo.SetOperationFlags(FILEOPERATION_FLAGS(flags))?;
    let dest_item: Option<IShellItem> = match dest {
        Some(d) => Some(SHCreateItemFromParsingName(&HSTRING::from(d), None)?),
        None => None,
    };
    for p in items {
        let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(p.as_path()), None)?;
        match (op, &dest_item) {
            (Op::Copy, Some(d)) => fo.CopyItem(&item, d, PCWSTR::null(), None)?,
            (Op::Move, Some(d)) => {
                if p.parent() != dest {
                    fo.MoveItem(&item, d, PCWSTR::null(), None)?
                }
            }
            (Op::Delete(_), _) => fo.DeleteItem(&item, None)?,
            _ => {}
        }
    }
    fo.PerformOperations()?;
    Ok(fo.GetAnyOperationsAborted()?.as_bool())
}
