//! Dragging clips out of the app, as files, into Discord, a browser, Explorer or anything else
//! that takes files dropped from Explorer. The files come from the shell itself (the same data
//! Explorer hands out), so every app that accepts an Explorer drag accepts this one.

use std::path::PathBuf;

use windows::Win32::Foundation::{DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, S_OK};
use windows::Win32::System::Com::IDataObject;
use windows::Win32::System::Ole::{DROPEFFECT, DROPEFFECT_COPY, DoDragDrop, IDropSource, IDropSource_Impl};
use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{BHID_DataObject, ILCreateFromPathW, ILFree, IShellItemArray, SHCreateShellItemArrayFromIDLists};
use windows::core::{BOOL, HRESULT, HSTRING, implement};

/// Tells Windows when the drag ends: dropped when the left button comes up, called off with Esc.
#[implement(IDropSource)]
struct Source;

impl IDropSource_Impl for Source_Impl {
    fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        if escape.as_bool() {
            DRAGDROP_S_CANCEL
        } else if keys.0 & MK_LBUTTON.0 == 0 {
            DRAGDROP_S_DROP
        } else {
            S_OK
        }
    }

    fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// Drags `paths` out as a copy, until the user drops them or presses Esc. Call on the window's
/// thread while the left button is down; it returns when the drag is over. Whether they were
/// dropped somewhere: Ok(true).
pub fn start(paths: &[PathBuf]) -> anyhow::Result<bool> {
    anyhow::ensure!(!paths.is_empty(), "no files");
    let ids: Vec<*const ITEMIDLIST> = paths.iter().map(|p| unsafe { ILCreateFromPathW(&HSTRING::from(p.as_os_str())) as *const ITEMIDLIST }).collect();
    let result = (|| -> anyhow::Result<bool> {
        anyhow::ensure!(ids.iter().all(|id| !id.is_null()), "a file isn't there any more");
        let data: IDataObject = unsafe {
            let items: IShellItemArray = SHCreateShellItemArrayFromIDLists(&ids)?;
            items.BindToHandler(None, &BHID_DataObject)?
        };
        let source: IDropSource = Source.into();
        let mut effect = DROPEFFECT::default();
        let hr = unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect) };
        Ok(hr == DRAGDROP_S_DROP)
    })();
    for id in ids {
        if !id.is_null() {
            unsafe { ILFree(Some(id)) };
        }
    }
    result
}

#[cfg(test)]
mod tests {
    /// A clip that was deleted under the app says so, before any drag starts.
    #[test]
    fn a_missing_clip_is_refused() {
        assert!(super::start(&[]).is_err());
        let gone = std::env::temp_dir().join("replay-vault-no-such-clip.mp4");
        let err = super::start(&[gone]).unwrap_err().to_string();
        assert!(err.contains("isn't there"), "{err}");
    }
}
