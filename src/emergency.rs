//! The emergency window: a plain Win32 process list with "End task".
//!
//! It uses no GPU and nothing from the main UI thread. It is the stock list
//! view control, drawn by GDI, on its own thread with its own message loop.
//! The hotkey (or a second launch) opens it when the main window has stopped
//! responding, for example because the graphics driver hung. Tasky also falls
//! back to it entirely when the main UI cannot start, e.g. without OpenGL.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::OnceLock;

use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, DeleteObject, GetSysColorBrush, InvalidateRect, COLOR_BTNFACE, HFONT,
    HGDIOBJ,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::{
    InitCommonControlsEx, ICC_LISTVIEW_CLASSES, INITCOMMONCONTROLSEX, LIST_VIEW_ITEM_STATE_FLAGS,
    LVCFMT_LEFT, LVCFMT_RIGHT, LVCF_FMT, LVCF_TEXT, LVCF_WIDTH, LVCOLUMNW, LVIF_TEXT, LVIS_FOCUSED,
    LVIS_SELECTED, LVITEMW, LVM_ENSUREVISIBLE, LVM_GETNEXTITEM, LVM_INSERTCOLUMNW,
    LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETITEMCOUNT, LVM_SETITEMSTATE, LVNI_SELECTED,
    LVN_GETDISPINFOW, LVN_KEYDOWN, LVSICF_NOSCROLL, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT,
    LVS_OWNERDATA, LVS_REPORT, LVS_SHOWSELALWAYS, LVS_SINGLESEL, NMHDR, NMLVDISPINFOW, NMLVKEYDOWN,
    WC_LISTVIEWW,
};
use windows::Win32::UI::HiDpi::{GetDpiForWindow, SystemParametersInfoForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::VK_DELETE;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    KillTimer, LoadCursorW, MessageBoxW, MoveWindow, PostQuitMessage, RegisterClassW, SendMessageW,
    SetForegroundWindow, SetTimer, SetWindowPos, SetWindowTextW, ShowWindow, TranslateMessage,
    BS_PUSHBUTTON, CW_USEDEFAULT, HMENU, IDC_ARROW, IDOK, MB_ICONERROR, MB_ICONWARNING, MB_OK,
    MB_OKCANCEL, MSG, NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS, SWP_NOMOVE, SWP_NOZORDER,
    SW_RESTORE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLOSE, WM_COMMAND, WM_CREATE, WM_DESTROY,
    WM_NOTIFY, WM_SETFONT, WM_SIZE, WM_TIMER, WNDCLASSW, WS_CHILD, WS_EX_CLIENTEDGE, WS_EX_TOPMOST,
    WS_OVERLAPPEDWINDOW, WS_TABSTOP, WS_VISIBLE,
};

use crate::collector::{Sampler, Snapshot};
use crate::fmt;
use crate::sys::control;

const CLASS: PCWSTR = w!("TaskyEmergencyWindow");
const ID_LIST: usize = 1;
const ID_END: usize = 2;
const TIMER_ID: usize = 1;
const TIMER_MS: u32 = 500;

static SAMPLER: OnceLock<Sampler> = OnceLock::new();
static OPEN: AtomicBool = AtomicBool::new(false);
static WINDOW: AtomicUsize = AtomicUsize::new(0);

/// Hand over the sampler. Until this is called, `open` does nothing.
pub fn init(sampler: Sampler) {
    let _ = SAMPLER.set(sampler);
}

/// Open the emergency window on a thread of its own, or focus it if it is
/// already open. Safe to call from any thread.
pub fn open() {
    if OPEN.swap(true, Ordering::SeqCst) {
        focus_existing();
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("tasky-emergency".into())
        .spawn(run);
    if spawned.is_err() {
        OPEN.store(false, Ordering::SeqCst);
    }
}

/// Run the emergency window on the calling thread until it is closed.
pub fn run_blocking() {
    if OPEN.swap(true, Ordering::SeqCst) {
        return;
    }
    run();
}

fn focus_existing() {
    let raw = WINDOW.load(Ordering::SeqCst);
    if raw != 0 {
        let hwnd = HWND(raw as *mut _);
        unsafe {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

fn run() {
    if let Some(sampler) = SAMPLER.get() {
        sampler.set_emergency(true);
        unsafe { create_and_pump() };
        sampler.set_emergency(false);
    }
    WINDOW.store(0, Ordering::SeqCst);
    OPEN.store(false, Ordering::SeqCst);
}

/// One row, with its cells already formatted as NUL-terminated UTF-16 for the
/// list view to copy from.
struct Line {
    pid: u32,
    name: String,
    cells: [Vec<u16>; 4],
}

struct Window {
    list: HWND,
    button: HWND,
    font: HFONT,
    dpi: u32,
    sequence: u64,
    lines: Vec<Line>,
}

thread_local! {
    static STATE: RefCell<Option<Window>> = const { RefCell::new(None) };
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn scale(value: i32, dpi: u32) -> i32 {
    value * dpi as i32 / 96
}

unsafe fn create_and_pump() {
    let hinstance = unsafe { GetModuleHandleW(None) }
        .map(|module| HINSTANCE(module.0))
        .unwrap_or_default();
    unsafe {
        let controls = INITCOMMONCONTROLSEX {
            dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_LISTVIEW_CLASSES,
        };
        let _ = InitCommonControlsEx(&controls);

        let class = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinstance,
            lpszClassName: CLASS,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: GetSysColorBrush(COLOR_BTNFACE),
            ..Default::default()
        };
        // Fails harmlessly when the window was opened before.
        RegisterClassW(&class);

        let Ok(hwnd) = CreateWindowExW(
            WS_EX_TOPMOST,
            CLASS,
            w!("Tasky (safe mode)"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            640,
            480,
            None,
            None,
            Some(hinstance),
            None,
        ) else {
            return;
        };
        WINDOW.store(hwnd.0 as usize, Ordering::SeqCst);

        let dpi = GetDpiForWindow(hwnd).max(96);
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            scale(640, dpi),
            scale(520, dpi),
            SWP_NOMOVE | SWP_NOZORDER,
        );
        let _ = SetForegroundWindow(hwnd);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        match msg {
            WM_CREATE => {
                create_children(hwnd);
                refresh(hwnd);
                SetTimer(Some(hwnd), TIMER_ID, TIMER_MS, None);
                LRESULT(0)
            }
            WM_SIZE => {
                layout(hwnd);
                LRESULT(0)
            }
            WM_TIMER => {
                refresh(hwnd);
                LRESULT(0)
            }
            WM_COMMAND if wparam.0 & 0xFFFF == ID_END => {
                end_selected(hwnd);
                LRESULT(0)
            }
            WM_NOTIFY => on_notify(hwnd, lparam),
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                let _ = KillTimer(Some(hwnd), TIMER_ID);
                if let Some(state) = STATE.with(|state| state.borrow_mut().take()) {
                    let _ = DeleteObject(HGDIOBJ(state.font.0));
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// The system's dialog font, at the window's DPI.
unsafe fn message_font(dpi: u32) -> HFONT {
    let mut metrics = NONCLIENTMETRICSW {
        cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
        ..Default::default()
    };
    unsafe {
        if SystemParametersInfoForDpi(
            SPI_GETNONCLIENTMETRICS.0,
            metrics.cbSize,
            Some(&mut metrics as *mut _ as *mut _),
            0,
            dpi,
        )
        .is_ok()
        {
            CreateFontIndirectW(&metrics.lfMessageFont)
        } else {
            HFONT::default()
        }
    }
}

unsafe fn create_children(hwnd: HWND) {
    unsafe {
        let hinstance = GetModuleHandleW(None)
            .map(|module| HINSTANCE(module.0))
            .ok();
        let dpi = GetDpiForWindow(hwnd).max(96);
        let font = message_font(dpi);

        let list = CreateWindowExW(
            WS_EX_CLIENTEDGE,
            WC_LISTVIEWW,
            PCWSTR::null(),
            WINDOW_STYLE(
                WS_CHILD.0
                    | WS_VISIBLE.0
                    | WS_TABSTOP.0
                    | LVS_REPORT
                    | LVS_OWNERDATA
                    | LVS_SINGLESEL
                    | LVS_SHOWSELALWAYS,
            ),
            0,
            0,
            0,
            0,
            Some(hwnd),
            Some(HMENU(ID_LIST as *mut _)),
            hinstance,
            None,
        )
        .unwrap_or_default();
        SendMessageW(
            list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            Some(WPARAM(0)),
            Some(LPARAM(
                (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER) as isize,
            )),
        );
        let columns = [
            ("Name", 260, LVCFMT_LEFT),
            ("PID", 70, LVCFMT_RIGHT),
            ("CPU", 70, LVCFMT_RIGHT),
            ("Memory", 100, LVCFMT_RIGHT),
        ];
        for (index, (title, width, format)) in columns.into_iter().enumerate() {
            let mut text = wide(title);
            let column = LVCOLUMNW {
                mask: LVCF_TEXT | LVCF_WIDTH | LVCF_FMT,
                fmt: format,
                cx: scale(width, dpi),
                pszText: PWSTR(text.as_mut_ptr()),
                ..Default::default()
            };
            SendMessageW(
                list,
                LVM_INSERTCOLUMNW,
                Some(WPARAM(index)),
                Some(LPARAM(&column as *const _ as isize)),
            );
        }

        let button = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            w!("End task"),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_PUSHBUTTON as u32),
            0,
            0,
            0,
            0,
            Some(hwnd),
            Some(HMENU(ID_END as *mut _)),
            hinstance,
            None,
        )
        .unwrap_or_default();

        for child in [list, button] {
            SendMessageW(
                child,
                WM_SETFONT,
                Some(WPARAM(font.0 as usize)),
                Some(LPARAM(1)),
            );
        }

        STATE.with(|state| {
            *state.borrow_mut() = Some(Window {
                list,
                button,
                font,
                dpi,
                sequence: 0,
                lines: Vec::new(),
            })
        });
        layout(hwnd);
    }
}

unsafe fn layout(hwnd: HWND) {
    let Some((list, button, dpi)) = STATE.with(|state| {
        state
            .borrow()
            .as_ref()
            .map(|window| (window.list, window.button, window.dpi))
    }) else {
        return;
    };
    let mut client = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut client);
        let (width, height) = (client.right, client.bottom);
        let margin = scale(8, dpi);
        let button_w = scale(110, dpi);
        let button_h = scale(28, dpi);
        let _ = MoveWindow(
            list,
            margin,
            margin,
            (width - 2 * margin).max(0),
            (height - 3 * margin - button_h).max(0),
            true,
        );
        let _ = MoveWindow(
            button,
            (width - margin - button_w).max(0),
            (height - margin - button_h).max(0),
            button_w,
            button_h,
            true,
        );
    }
}

fn selected_index(list: HWND) -> Option<usize> {
    let index = unsafe {
        SendMessageW(
            list,
            LVM_GETNEXTITEM,
            Some(WPARAM(usize::MAX)),
            Some(LPARAM(LVNI_SELECTED as isize)),
        )
    };
    usize::try_from(index.0).ok()
}

fn lines_from(snapshot: &Snapshot) -> Vec<Line> {
    let mut rows: Vec<_> = snapshot.rows.iter().collect();
    rows.sort_by(|a, b| b.cpu.total_cmp(&a.cpu).then(b.memory.cmp(&a.memory)));
    rows.into_iter()
        .map(|row| Line {
            pid: row.pid,
            name: row.info.name.clone(),
            cells: [
                wide(&row.info.name),
                wide(&row.pid.to_string()),
                wide(&fmt::pct(row.cpu, 1)),
                wide(&fmt::bytes(row.memory)),
            ],
        })
        .collect()
}

/// Swap in the newest snapshot, keeping the selected process selected.
///
/// No `RefCell` borrow is held across a `SendMessageW` to the list view,
/// because the list view calls straight back into `wndproc` for cell text.
unsafe fn refresh(hwnd: HWND) {
    let Some(snapshot) = SAMPLER.get().and_then(Sampler::latest) else {
        return;
    };
    let Some(list) = STATE.with(|state| {
        state
            .borrow()
            .as_ref()
            .filter(|window| window.sequence != snapshot.sequence)
            .map(|window| window.list)
    }) else {
        return;
    };

    let selected = selected_index(list);
    let lines = lines_from(&snapshot);
    let count = lines.len();
    let reselect = STATE.with(|state| {
        let mut state = state.borrow_mut();
        let window = state.as_mut()?;
        let pid = selected
            .and_then(|index| window.lines.get(index))
            .map(|line| line.pid);
        window.lines = lines;
        window.sequence = snapshot.sequence;
        pid.and_then(|pid| window.lines.iter().position(|line| line.pid == pid))
    });

    unsafe {
        SendMessageW(
            list,
            LVM_SETITEMCOUNT,
            Some(WPARAM(count)),
            Some(LPARAM(LVSICF_NOSCROLL as isize)),
        );
        if let Some(index) = reselect {
            let item = LVITEMW {
                stateMask: LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0),
                state: LIST_VIEW_ITEM_STATE_FLAGS(LVIS_SELECTED.0 | LVIS_FOCUSED.0),
                ..Default::default()
            };
            SendMessageW(
                list,
                LVM_SETITEMSTATE,
                Some(WPARAM(index)),
                Some(LPARAM(&item as *const _ as isize)),
            );
            SendMessageW(
                list,
                LVM_ENSUREVISIBLE,
                Some(WPARAM(index)),
                Some(LPARAM(0)),
            );
        }
        let _ = InvalidateRect(Some(list), None, false);

        let totals = &snapshot.totals;
        let title = format!(
            "Tasky (safe mode)  ·  CPU {:.0}%  ·  RAM {:.0}%  ·  {} processes",
            totals.cpu,
            totals.memory_percent(),
            totals.processes
        );
        let _ = SetWindowTextW(hwnd, &HSTRING::from(title));
    }
}

unsafe fn on_notify(hwnd: HWND, lparam: LPARAM) -> LRESULT {
    unsafe {
        let header = &*(lparam.0 as *const NMHDR);
        match header.code {
            LVN_GETDISPINFOW => {
                let info = &mut *(lparam.0 as *mut NMLVDISPINFOW);
                if info.item.mask.0 & LVIF_TEXT.0 != 0 && info.item.cchTextMax > 0 {
                    fill_cell(&mut info.item);
                }
            }
            LVN_KEYDOWN => {
                // Declared with 1-byte packing in commctrl.h.
                let key = std::ptr::read_unaligned(lparam.0 as *const NMLVKEYDOWN).wVKey;
                if key == VK_DELETE.0 {
                    end_selected(hwnd);
                }
            }
            _ => {}
        }
        LRESULT(0)
    }
}

unsafe fn fill_cell(item: &mut LVITEMW) {
    STATE.with(|state| {
        // Leave the cell empty rather than panic on a re-entrant call.
        let Ok(state) = state.try_borrow() else {
            return;
        };
        let text = state
            .as_ref()
            .and_then(|window| window.lines.get(usize::try_from(item.iItem).ok()?))
            .and_then(|line| line.cells.get(usize::try_from(item.iSubItem).ok()?));
        let Some(text) = text else {
            return;
        };
        // Copy at most cchTextMax - 1 units, and always terminate.
        let max = item.cchTextMax as usize - 1;
        let len = (text.len() - 1).min(max);
        unsafe {
            std::ptr::copy_nonoverlapping(text.as_ptr(), item.pszText.0, len);
            *item.pszText.0.add(len) = 0;
        }
    });
}

unsafe fn end_selected(hwnd: HWND) {
    let Some(list) = STATE.with(|state| state.borrow().as_ref().map(|window| window.list)) else {
        return;
    };
    let Some(index) = selected_index(list) else {
        return;
    };
    let Some((pid, name)) = STATE.with(|state| {
        state
            .borrow()
            .as_ref()
            .and_then(|window| window.lines.get(index))
            .map(|line| (line.pid, line.name.clone()))
    }) else {
        return;
    };
    if pid <= 4 || pid == std::process::id() {
        return;
    }

    let question = HSTRING::from(format!(
        "End {name} (PID {pid})?\n\nUnsaved data in it will be lost."
    ));
    let answer = unsafe {
        MessageBoxW(
            Some(hwnd),
            PCWSTR(question.as_ptr()),
            w!("Tasky"),
            MB_OKCANCEL | MB_ICONWARNING,
        )
    };
    if answer != IDOK {
        return;
    }
    if let Some(error) = control::kill(&[pid]).into_iter().next() {
        let message = HSTRING::from(format!("Could not end PID {pid}: {}", error.message));
        unsafe {
            MessageBoxW(
                Some(hwnd),
                PCWSTR(message.as_ptr()),
                w!("Tasky"),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}
