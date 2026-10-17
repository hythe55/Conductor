//! The shape of the mouse pointer, as a CSS cursor the client shows over the
//! stream. Captured frames don't include the pointer, and a client's own
//! pointer would otherwise stay an arrow over text, links and window edges.

use windows::Win32::UI::WindowsAndMessaging::{
    CURSOR_SHOWING, CURSOR_SUPPRESSED, CURSORINFO, GetCursorInfo, IDC_APPSTARTING, IDC_ARROW, IDC_CROSS, IDC_HAND,
    IDC_HELP, IDC_IBEAM, IDC_NO, IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, IDC_WAIT,
    LoadCursorW,
};
use windows::core::PCWSTR;

const SHAPES: [(PCWSTR, &str); 13] = [
    (IDC_ARROW, "default"),
    (IDC_IBEAM, "text"),
    (IDC_HAND, "pointer"),
    (IDC_WAIT, "wait"),
    (IDC_APPSTARTING, "progress"),
    (IDC_CROSS, "crosshair"),
    (IDC_HELP, "help"),
    (IDC_NO, "not-allowed"),
    (IDC_SIZEALL, "move"),
    (IDC_SIZEWE, "ew-resize"),
    (IDC_SIZENS, "ns-resize"),
    (IDC_SIZENWSE, "nwse-resize"),
    (IDC_SIZENESW, "nesw-resize"),
];

pub struct Cursor {
    /// The system's standard pointers, by handle.
    known: Vec<(usize, &'static str)>,
    last: &'static str,
}

impl Cursor {
    pub fn new() -> Self {
        let known = SHAPES
            .iter()
            .filter_map(|&(id, name)| unsafe { LoadCursorW(None, id) }.ok().map(|h| (h.0 as usize, name)))
            .collect();
        // Nothing reported yet, so the first call always says what the pointer is.
        Self { known, last: "" }
    }

    /// The pointer's CSS cursor, if it changed since the last call. Pointers
    /// apps draw themselves show as the arrow.
    pub fn changed(&mut self) -> Option<&'static str> {
        let mut info = CURSORINFO { cbSize: size_of::<CURSORINFO>() as u32, ..Default::default() };
        // Fails while the sign-in or UAC screen is up; keep the last shape.
        unsafe { GetCursorInfo(&mut info) }.ok()?;
        // Windows also stops drawing the pointer while someone uses touch or a pen
        // there; the person controlling still needs to see it.
        let hidden = info.flags.0 & (CURSOR_SHOWING.0 | CURSOR_SUPPRESSED.0) == 0;
        let name = if hidden {
            "none"
        } else {
            let handle = info.hCursor.0 as usize;
            self.known.iter().find(|(h, _)| *h == handle).map_or("default", |(_, name)| name)
        };
        (name != self.last).then(|| {
            self.last = name;
            name
        })
    }
}
