//! A small banner at the top of a display while another device controls it,
//! or a cover over every display while a session hides the screen.
//!
//! Both are excluded from screen capture, so the controlling device never
//! sees them, and they ignore the mouse, so clicks pass through to whatever is
//! underneath. They live on their own thread with a message loop, on the
//! desktop the process started on (the user's normal desktop). Windows keeps
//! some things above any app's window, such as the Start menu, so the cover
//! hides most of the screen rather than all of it. While the cover is up, the
//! PC's own keyboard and mouse are off too (see [`crate::block`]).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, Once};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateRoundRectRgn, CreateSolidBrush,
    DEFAULT_CHARSET, DT_CENTER, DT_SINGLELINE, DT_VCENTER, DeleteObject, DrawTextW, EndPaint, FW_NORMAL, FillRect,
    GetDC, GetTextExtentPoint32W, HFONT, InvalidateRect, MONITOR_DEFAULTTONEAREST, MonitorFromRect,
    OUT_DEFAULT_PRECIS, PAINTSTRUCT, ReleaseDC, SelectObject, SetBkMode, SetTextColor, SetWindowRgn, TRANSPARENT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetClientRect, GetMessageW,
    GetWindowLongPtrW, GetWindowTextW, HTTRANSPARENT, HWND_TOPMOST, LWA_ALPHA, MA_NOACTIVATE, MSG, PM_NOREMOVE,
    PeekMessageW, PostThreadMessageW, RegisterClassW, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetLayeredWindowAttributes,
    SetWindowDisplayAffinity, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, TranslateMessage,
    WDA_EXCLUDEFROMCAPTURE, WM_APP, WM_DISPLAYCHANGE, WM_MOUSEACTIVATE, WM_NCHITTEST, WM_PAINT, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{PCWSTR, w};

const WM_REFRESH: u32 = WM_APP + 1;
const CLASS: PCWSTR = w!("ConductorBanner");

// Matches the session menu's look: dark surface, light text.
const BACKGROUND: COLORREF = COLORREF(0x0026_2626);
const TEXT: COLORREF = COLORREF(0x00F2_F2F2);
const OPACITY: u8 = 235;
const COVER_BACKGROUND: COLORREF = COLORREF(0);
const COVER_TEXT: COLORREF = COLORREF(0x00A3_A3A3);
/// Sizes at 100% scaling.
const FONT_PX: i32 = 13;
const COVER_FONT_PX: i32 = 18;
/// Set in a window's user data, next to its DPI, when it's a cover.
const COVER_FLAG: isize = 1 << 16;
const HEIGHT_PX: i32 = 28;
const PADDING_PX: i32 = 14;
const RADIUS_PX: i32 = 8;

/// A display rectangle in physical desktop coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Area {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

struct Entry {
    id: u64,
    controller: String,
    area: Option<Area>,
    /// The session hides the screen.
    private: bool,
}

static ENTRIES: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
static THREAD: AtomicU32 = AtomicU32::new(0);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static START: Once = Once::new();

/// Shows "Controlled from <device>" on a display for as long as it's alive.
pub struct Banner {
    id: u64,
}

impl Banner {
    pub fn new(controller: String) -> Self {
        START.call_once(|| {
            std::thread::Builder::new()
                .name("banner".into())
                .spawn(run)
                .map_err(|e| tracing::warn!("banner thread: {e}"))
                .ok();
        });
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        ENTRIES.lock().unwrap().push(Entry { id, controller, area: None, private: false });
        Self { id }
    }

    /// Moves the banner to the display now being controlled.
    pub fn set_area(&self, area: Area) {
        if let Some(e) = ENTRIES.lock().unwrap().iter_mut().find(|e| e.id == self.id) {
            e.area = Some(area);
        }
        refresh();
    }

    /// Covers every display so nobody at this PC sees what the session does.
    pub fn hide_screen(&self, hide: bool) {
        if let Some(e) = ENTRIES.lock().unwrap().iter_mut().find(|e| e.id == self.id) {
            e.private = hide;
        }
        refresh();
    }
}

impl Drop for Banner {
    fn drop(&mut self) {
        ENTRIES.lock().unwrap().retain(|e| e.id != self.id);
        refresh();
    }
}

fn refresh() {
    let thread = THREAD.load(Ordering::Acquire);
    if thread != 0 {
        _ = unsafe { PostThreadMessageW(thread, WM_REFRESH, WPARAM(0), LPARAM(0)) };
    }
}

fn run() {
    unsafe {
        let Ok(instance) = GetModuleHandleW(None) else { return };
        let class = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: instance.into(), lpszClassName: CLASS, ..Default::default() };
        if RegisterClassW(&class) == 0 {
            tracing::warn!("banner: couldn't register the window class");
            return;
        }
        // Create this thread's message queue before announcing it.
        let mut msg = MSG::default();
        _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        THREAD.store(GetCurrentThreadId(), Ordering::Release);

        let mut windows = HashMap::new();
        let mut supported = true;
        reconcile(&mut windows, &mut supported);
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.hwnd.is_invalid() && msg.message == WM_REFRESH {
                reconcile(&mut windows, &mut supported);
                continue;
            }
            _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// The windows the sessions call for, by display and whether it's a cover,
/// with who's controlling: a banner on each controlled display, or a cover
/// on every display while any session hides the screen.
fn wanted() -> HashMap<(Area, bool), Vec<String>> {
    let (private, entries) = {
        let entries = ENTRIES.lock().unwrap();
        let private = entries.iter().any(|e| e.private);
        (private, entries.iter().map(|e| (e.area, e.controller.clone())).collect::<Vec<_>>())
    };
    let mut wanted: HashMap<(Area, bool), Vec<String>> = HashMap::new();
    if private {
        let mut names: Vec<String> = Vec::new();
        for (_, name) in &entries {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        let mut areas: Vec<Area> = match crate::capture::displays() {
            Ok(displays) => displays.iter().map(|d| Area { x: d.x, y: d.y, width: d.width, height: d.height }).collect(),
            Err(e) => {
                tracing::warn!("cover: couldn't list displays: {e:#}");
                Vec::new()
            }
        };
        // At least cover the displays being controlled.
        if areas.is_empty() {
            areas = entries.iter().filter_map(|(area, _)| *area).collect();
        }
        for area in areas {
            wanted.insert((area, true), names.clone());
        }
    } else {
        for (area, name) in entries {
            if let Some(area) = area {
                let names = wanted.entry((area, false)).or_default();
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    wanted
}

/// Makes the windows match the active sessions.
fn reconcile(windows: &mut HashMap<(Area, bool), HWND>, supported: &mut bool) {
    let wanted = wanted();
    // Only while the cover says so, so whoever is at the PC knows why.
    let blocked = crate::block::set(*supported && wanted.keys().any(|(_, cover)| *cover));
    windows.retain(|key, hwnd| {
        let keep = wanted.contains_key(key);
        if !keep {
            _ = unsafe { DestroyWindow(*hwnd) };
        }
        keep
    });
    if !*supported {
        return;
    }
    for ((area, cover), names) in wanted {
        let hwnd = match windows.get(&(area, cover)) {
            Some(hwnd) => *hwnd,
            None => match create(cover) {
                Ok(hwnd) => *windows.entry((area, cover)).or_insert(hwnd),
                Err(e) => {
                    // Older Windows can't hide a window from capture. Showing it
                    // would put it on the controlling device's screen, so don't.
                    tracing::info!("banner unavailable: {e:#}");
                    *supported = false;
                    crate::block::set(false);
                    return;
                }
            },
        };
        let text = match (cover, blocked) {
            (false, _) => label(&names),
            (true, false) => format!("{}. This screen is hidden.", label(&names)),
            (true, true) => format!("{}. This screen is hidden, and its keyboard and mouse are off.", label(&names)),
        };
        tracing::debug!("{} on {area:?}: {text}", if cover { "cover" } else { "banner" });
        place(hwnd, area, &text, cover);
    }
}

fn label(names: &[String]) -> String {
    match names {
        [one] => format!("Controlled from {one}"),
        [a, b] => format!("Controlled from {a} and {b}"),
        _ => format!("Controlled from {} devices", names.len()),
    }
}

fn create(cover: bool) -> anyhow::Result<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None)?;
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED | WS_EX_TRANSPARENT,
            CLASS,
            w!(""),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )?;
        // Windows 10 refuses to exclude a layered window from capture (with a
        // misleading out-of-memory error) until its transparency is set.
        let opacity = if cover { 255 } else { OPACITY };
        let excluded = SetLayeredWindowAttributes(hwnd, COLORREF(0), opacity, LWA_ALPHA)
            .and_then(|()| SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE));
        if let Err(e) = excluded {
            _ = DestroyWindow(hwnd);
            return Err(anyhow::Error::from(e).context("excluding the banner from capture"));
        }
        Ok(hwnd)
    }
}

fn place(hwnd: HWND, area: Area, text: &str, cover: bool) {
    unsafe {
        let rect = RECT {
            left: area.x,
            top: area.y,
            right: area.x + area.width as i32,
            bottom: area.y + area.height as i32,
        };
        let monitor = MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST);
        let (mut dpi, mut _dpi_y) = (96u32, 96u32);
        _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut _dpi_y);
        let dpi = dpi as i32;
        let scale = |px: i32| px * dpi / 96;
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, dpi as isize | if cover { COVER_FLAG } else { 0 });

        let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        _ = SetWindowTextW(hwnd, PCWSTR(wide.as_ptr()));

        if cover {
            SetWindowRgn(hwnd, None, false);
            let (w, h) = (area.width as i32, area.height as i32);
            _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), area.x, area.y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            _ = InvalidateRect(Some(hwnd), None, true);
            return;
        }

        let font = font(dpi, FONT_PX);
        let hdc = GetDC(Some(hwnd));
        let old = SelectObject(hdc, font.into());
        let mut size = Default::default();
        _ = GetTextExtentPoint32W(hdc, &wide[..wide.len() - 1], &mut size);
        SelectObject(hdc, old);
        ReleaseDC(Some(hwnd), hdc);
        _ = DeleteObject(font.into());

        let width = size.cx + 2 * scale(PADDING_PX);
        let height = scale(HEIGHT_PX);
        let x = area.x + (area.width as i32 - width) / 2;
        // Rounded bottom corners only: the top edge sits flush with the screen edge.
        let radius = scale(RADIUS_PX);
        let region = CreateRoundRectRgn(0, -radius, width + 1, height + 1, radius * 2, radius * 2);
        SetWindowRgn(hwnd, Some(region), false);
        _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, area.y, width, height, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        _ = InvalidateRect(Some(hwnd), None, true);
    }
}

fn font(dpi: i32, px: i32) -> HFONT {
    unsafe {
        CreateFontW(
            -(px * dpi / 96),
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            w!("Segoe UI"),
        )
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            unsafe { paint(hwnd) };
            LRESULT(0)
        }
        // Never take clicks or focus from the person using this PC.
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        // Covers follow the displays as they're added, removed or resized.
        WM_DISPLAYCHANGE => {
            refresh();
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

unsafe fn paint(hwnd: HWND) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rect = RECT::default();
        _ = GetClientRect(hwnd, &mut rect);
        let data = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
        let cover = data & COVER_FLAG != 0;
        let brush = CreateSolidBrush(if cover { COVER_BACKGROUND } else { BACKGROUND });
        FillRect(hdc, &rect, brush);
        _ = DeleteObject(brush.into());

        let mut text = [0u16; 256];
        let len = GetWindowTextW(hwnd, &mut text) as usize;
        let dpi = (data & 0xFFFF) as i32;
        let font = font(if dpi > 0 { dpi } else { 96 }, if cover { COVER_FONT_PX } else { FONT_PX });
        let old = SelectObject(hdc, font.into());
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, if cover { COVER_TEXT } else { TEXT });
        DrawTextW(hdc, &mut text[..len], &mut rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        SelectObject(hdc, old);
        _ = DeleteObject(font.into());
        _ = EndPaint(hwnd, &ps);
    }
}
