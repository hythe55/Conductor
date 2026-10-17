//! A small banner at the top of a display while another device controls it.
//!
//! The banner is excluded from screen capture, so the controlling device never
//! sees it, and it ignores the mouse, so clicks pass through to whatever is
//! underneath. It lives on its own thread with a message loop, on the desktop
//! the process started on (the user's normal desktop).

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
    WDA_EXCLUDEFROMCAPTURE, WM_APP, WM_MOUSEACTIVATE, WM_NCHITTEST, WM_PAINT, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{PCWSTR, w};

const WM_REFRESH: u32 = WM_APP + 1;
const CLASS: PCWSTR = w!("ConductorBanner");

// Matches the session menu's look: dark surface, light text.
const BACKGROUND: COLORREF = COLORREF(0x0026_2626);
const TEXT: COLORREF = COLORREF(0x00F2_F2F2);
const OPACITY: u8 = 235;
/// Sizes at 100% scaling.
const FONT_PX: i32 = 13;
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
        ENTRIES.lock().unwrap().push(Entry { id, controller, area: None });
        Self { id }
    }

    /// Moves the banner to the display now being controlled.
    pub fn set_area(&self, area: Area) {
        if let Some(e) = ENTRIES.lock().unwrap().iter_mut().find(|e| e.id == self.id) {
            e.area = Some(area);
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

/// Makes the banner windows match the active sessions: one per controlled display.
fn reconcile(windows: &mut HashMap<Area, HWND>, supported: &mut bool) {
    let mut wanted: HashMap<Area, Vec<String>> = HashMap::new();
    for e in ENTRIES.lock().unwrap().iter() {
        if let Some(area) = e.area {
            let names = wanted.entry(area).or_default();
            if !names.contains(&e.controller) {
                names.push(e.controller.clone());
            }
        }
    }
    windows.retain(|area, hwnd| {
        let keep = wanted.contains_key(area);
        if !keep {
            _ = unsafe { DestroyWindow(*hwnd) };
        }
        keep
    });
    if !*supported {
        return;
    }
    for (area, names) in wanted {
        let hwnd = match windows.get(&area) {
            Some(hwnd) => *hwnd,
            None => match create() {
                Ok(hwnd) => *windows.entry(area).or_insert(hwnd),
                Err(e) => {
                    // Older Windows can't hide a window from capture. Showing it
                    // would put it on the controlling device's screen, so don't.
                    tracing::info!("banner unavailable: {e:#}");
                    *supported = false;
                    return;
                }
            },
        };
        let text = label(&names);
        tracing::debug!("banner on {area:?}: {text}");
        place(hwnd, area, &text);
    }
}

fn label(names: &[String]) -> String {
    match names {
        [one] => format!("Controlled from {one}"),
        [a, b] => format!("Controlled from {a} and {b}"),
        _ => format!("Controlled from {} devices", names.len()),
    }
}

fn create() -> anyhow::Result<HWND> {
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
        let excluded = SetLayeredWindowAttributes(hwnd, COLORREF(0), OPACITY, LWA_ALPHA)
            .and_then(|()| SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE));
        if let Err(e) = excluded {
            _ = DestroyWindow(hwnd);
            return Err(anyhow::Error::from(e).context("excluding the banner from capture"));
        }
        Ok(hwnd)
    }
}

fn place(hwnd: HWND, area: Area, text: &str) {
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
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, dpi as isize);

        let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        _ = SetWindowTextW(hwnd, PCWSTR(wide.as_ptr()));

        let font = font(dpi);
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

fn font(dpi: i32) -> HFONT {
    unsafe {
        CreateFontW(
            -(FONT_PX * dpi / 96),
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
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

unsafe fn paint(hwnd: HWND) {
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        let mut rect = RECT::default();
        _ = GetClientRect(hwnd, &mut rect);
        let brush = CreateSolidBrush(BACKGROUND);
        FillRect(hdc, &rect, brush);
        _ = DeleteObject(brush.into());

        let mut text = [0u16; 256];
        let len = GetWindowTextW(hwnd, &mut text) as usize;
        let dpi = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as i32;
        let font = font(if dpi > 0 { dpi } else { 96 });
        let old = SelectObject(hdc, font.into());
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, TEXT);
        DrawTextW(hdc, &mut text[..len], &mut rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE);
        SelectObject(hdc, old);
        _ = DeleteObject(font.into());
        _ = EndPaint(hwnd, &ps);
    }
}
