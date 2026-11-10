use std::sync::{Mutex, PoisonError};

// Kept for the life of the process: on X11 the owning process serves the
// selection, so dropping the handle right after set_text clears the clipboard
// unless a clipboard manager happens to grab it first.
static CLIPBOARD: Mutex<Option<arboard::Clipboard>> = Mutex::new(None);

/// Copies text to the system clipboard.
pub fn copy(text: &str) -> Result<(), arboard::Error> {
    let mut guard = CLIPBOARD.lock().unwrap_or_else(PoisonError::into_inner);
    if guard.is_none() {
        *guard = Some(arboard::Clipboard::new()?);
    }
    let result = guard.as_mut().unwrap().set_text(text);
    if result.is_err() {
        *guard = None;
    }
    result
}
