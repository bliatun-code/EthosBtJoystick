//! Notification area ("system tray") icon.
//!
//! The icon belongs to a small hidden window of our own, created on the GUI
//! thread; its messages are dispatched by the normal event loop. A second
//! instance of the program finds that window by its class name and asks it to
//! show the main window (see `main.rs`).
//!
//! "In the notification area" does not mean hidden - see [`send_to_tray`].

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{BOOL, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_CLOAK};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    DefSubclassProc, ITaskbarList, RemoveWindowSubclass, SetWindowSubclass, Shell_NotifyIconW, TaskbarList, NIF_ICON,
    NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

/// Class name of the hidden window (used to find a running instance).
pub const WINDOW_CLASS: PCWSTR = w!("EthosBtJoystick.Tray");
/// Posted to the hidden window to bring the main window back.
pub const WM_SHOW: u32 = WM_APP + 2;
/// Posted to the hidden window to end the program (same as "Exit" in the menu).
pub const WM_EXIT: u32 = WM_APP + 3;

const WM_TRAY: u32 = WM_APP + 1;
const ICON_ID: u32 = 1;
const MENU_SHOW: usize = 1;
const MENU_EXIT: usize = 2;
const SUBCLASS_ID: usize = 0xEB1;

/// Values for the shared language cell (the menu follows the GUI language).
pub const LANG_NORWEGIAN: u8 = 0;
pub const LANG_ENGLISH: u8 = 1;

struct State {
    main: HWND,
    icon: HICON,
    lang: Arc<AtomicU8>,
    /// Called for "Exit" in the menu. Expected not to return.
    on_exit: Box<dyn Fn()>,
    flags: CloseFlags,
    /// Broadcast by Explorer when the taskbar is (re)created.
    taskbar_created: u32,
}

/// How the main window's close button is handled; shared with the GUI.
#[derive(Clone, Default)]
pub struct CloseFlags {
    /// Set by the GUI: the close button hides the window instead of ending
    /// the program.
    pub close_to_tray: Arc<AtomicBool>,
    /// Set here when the close button was pressed in that mode; the GUI then
    /// hides the window.
    pub hide_requested: Arc<AtomicBool>,
}

/// The tray icon. Must be created and dropped on the GUI thread.
pub struct Tray {
    hwnd: HWND,
    main: HWND,
    state: *mut State,
    intercepts_close: bool,
}

/// Lets other threads update the icon's tooltip.
#[derive(Clone, Copy)]
pub struct TrayHandle(isize);

impl Tray {
    /// `main` is the main window's HWND, `rgba` a square icon image.
    pub fn new(
        main: isize,
        rgba: &[u8],
        size: u32,
        lang: Arc<AtomicU8>,
        flags: CloseFlags,
        on_exit: Box<dyn Fn()>,
    ) -> Option<Self> {
        unsafe {
            let instance: HINSTANCE = GetModuleHandleW(None).ok()?.into();
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: WINDOW_CLASS,
                ..Default::default()
            };
            RegisterClassW(&class);

            // an ordinary top-level window that is simply never shown (a
            // message-only window would not get the TaskbarCreated broadcast)
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                WINDOW_CLASS,
                w!("Ethos BT Joystick"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                HWND::default(),
                HMENU::default(),
                instance,
                None,
            )
            .ok()?;

            let state = Box::into_raw(Box::new(State {
                main: HWND(main as _),
                icon: create_icon(instance, rgba, size),
                lang,
                on_exit,
                flags,
                taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
            }));
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);

            // see the close button before the GUI framework does (main_window_proc)
            let main = HWND(main as _);
            let intercepts_close =
                SetWindowSubclass(main, Some(main_window_proc), SUBCLASS_ID, state as usize).as_bool();

            add_icon(hwnd, &*state);
            Some(Self { hwnd, main, state, intercepts_close })
        }
    }

    /// Whether "close to tray" can work (the close button is intercepted).
    pub fn intercepts_close(&self) -> bool {
        self.intercepts_close
    }

    pub fn handle(&self) -> TrayHandle {
        TrayHandle(self.hwnd.0 as isize)
    }

    /// Shows a notification bubble next to the icon.
    pub fn show_notice(&self, title: &str, text: &str) {
        let mut data = notify_data(self.hwnd);
        data.uFlags = NIF_INFO;
        data.dwInfoFlags = NIIF_INFO;
        copy_text(&mut data.szInfoTitle, title);
        copy_text(&mut data.szInfo, text);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
        }
    }

    /// Removes the icon right away (used just before the process exits).
    pub fn remove_icon(hwnd: isize) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &notify_data(HWND(hwnd as _)));
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &notify_data(self.hwnd));
            let _ = RemoveWindowSubclass(self.main, Some(main_window_proc), SUBCLASS_ID);
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.hwnd);
            let state = Box::from_raw(self.state);
            if !state.icon.is_invalid() {
                let _ = DestroyIcon(state.icon);
            }
        }
    }
}

impl TrayHandle {
    pub fn raw(self) -> isize {
        self.0
    }

    /// May be called from any thread.
    pub fn set_tooltip(self, text: &str) {
        let mut data = notify_data(HWND(self.0 as _));
        data.uFlags = NIF_TIP;
        copy_text(&mut data.szTip, text);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
        }
    }
}

/// Brings the main window back from the notification area (or just to the
/// front if it was not there).
pub fn show_window(main: isize) {
    let main = HWND(main as _);
    unsafe {
        set_cloaked(main, false);
        set_taskbar_button(main, true);
        let _ = ShowWindow(main, if IsIconic(main).as_bool() { SW_RESTORE } else { SW_SHOW });
        let _ = SetForegroundWindow(main);
    }
}

/// Puts the main window "in the notification area".
///
/// The window is *not* hidden: eframe's event loop spins at 100 % CPU as soon
/// as a hidden window is asked to redraw (measured, and any focus change asks
/// for that). A minimised window keeps receiving its redraws, so instead it is
/// minimised, its taskbar button is removed, and it is cloaked so that it does
/// not show up in Alt+Tab either.
pub fn send_to_tray(main: isize) {
    let main = HWND(main as _);
    unsafe {
        set_cloaked(main, true);
        let _ = ShowWindow(main, SW_MINIMIZE);
        set_taskbar_button(main, false);
    }
}

/// Makes the window invisible on screen without hiding it (used before the
/// very first frame of a minimised start, so the window never flashes up).
pub fn cloak_window(main: isize) {
    unsafe { set_cloaked(HWND(main as _), true) }
}

unsafe fn set_cloaked(window: HWND, cloaked: bool) {
    let value = BOOL::from(cloaked);
    let _ = DwmSetWindowAttribute(window, DWMWA_CLOAK, &value as *const BOOL as _, std::mem::size_of::<BOOL>() as u32);
}

unsafe fn set_taskbar_button(window: HWND, present: bool) {
    let Ok(taskbar) = CoCreateInstance::<_, ITaskbarList>(&TaskbarList, None, CLSCTX_INPROC_SERVER) else { return };
    if taskbar.HrInit().is_err() {
        return;
    }
    let _ = if present { taskbar.AddTab(window) } else { taskbar.DeleteTab(window) };
}

/// Asks an already running instance to show its window. Returns false if no
/// running instance was found.
pub fn activate_running_instance() -> bool {
    unsafe {
        let Ok(hwnd) = FindWindowW(WINDOW_CLASS, PCWSTR::null()) else { return false };
        if hwnd.is_invalid() {
            return false;
        }
        // we were just started by the user, so we may pass on the right to
        // take the foreground
        let mut process = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut process));
        let _ = AllowSetForegroundWindow(process);
        PostMessageW(hwnd, WM_SHOW, WPARAM(0), LPARAM(0)).is_ok()
    }
}

fn notify_data(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ICON_ID,
        ..Default::default()
    }
}

/// Copies `text` into a fixed-size, NUL terminated UTF-16 buffer.
fn copy_text(buffer: &mut [u16], text: &str) {
    let max = buffer.len() - 1;
    let mut len = 0;
    for unit in text.encode_utf16().take(max) {
        buffer[len] = unit;
        len += 1;
    }
    buffer[len] = 0;
}

unsafe fn add_icon(hwnd: HWND, state: &State) {
    let mut data = notify_data(hwnd);
    data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    data.uCallbackMessage = WM_TRAY;
    data.hIcon = state.icon;
    copy_text(&mut data.szTip, "Ethos BT Joystick");
    let _ = Shell_NotifyIconW(NIM_ADD, &data);
}

/// Builds an icon from straight-alpha RGBA pixels.
unsafe fn create_icon(instance: HINSTANCE, rgba: &[u8], size: u32) -> HICON {
    let mut bgra = rgba.to_vec();
    for pixel in bgra.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    // the AND mask is ignored for 32 bit icons with an alpha channel
    let and_mask = vec![0u8; (size * size / 8) as usize];
    CreateIcon(instance, size as i32, size as i32, 1, 32, and_mask.as_ptr(), bgra.as_ptr()).unwrap_or_default()
}

unsafe fn popup_menu(hwnd: HWND, state: &State) {
    let english = state.lang.load(Ordering::Relaxed) == LANG_ENGLISH;
    let (show, exit) = if english { (w!("Open"), w!("Exit")) } else { (w!("Åpne"), w!("Avslutt")) };

    let Ok(menu) = CreatePopupMenu() else { return };
    let _ = AppendMenuW(menu, MF_STRING, MENU_SHOW, show);
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, MENU_EXIT, exit);
    let _ = SetMenuDefaultItem(menu, MENU_SHOW as u32, 0);

    let mut cursor = POINT::default();
    let _ = GetCursorPos(&mut cursor);
    // without this the menu would not close when the user clicks elsewhere
    let _ = SetForegroundWindow(hwnd);
    let command = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
        cursor.x,
        cursor.y,
        0,
        hwnd,
        None,
    );
    let _ = PostMessageW(hwnd, WM_NULL, WPARAM(0), LPARAM(0));
    let _ = DestroyMenu(menu);

    match command.0 as usize {
        MENU_SHOW => show_window(state.main.0 as isize),
        MENU_EXIT => (state.on_exit)(),
        _ => {}
    }
}

/// Subclass of the main window. With "close to tray" on, the close button
/// (and Alt+F4) is swallowed here, before the GUI framework starts its own
/// close handling, and turned into a plain "please go to the tray" for the
/// GUI - followed by a redraw so the GUI gets to see the request.
unsafe extern "system" fn main_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    state: usize,
) -> LRESULT {
    let state = &*(state as *const State);
    if message == WM_CLOSE && state.flags.close_to_tray.load(Ordering::Relaxed) {
        state.flags.hide_requested.store(true, Ordering::Relaxed);
        let _ = InvalidateRect(hwnd, None, false);
        return LRESULT(0);
    }
    DefSubclassProc(hwnd, message, wparam, lparam)
}

unsafe extern "system" fn window_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let state = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const State;
    if state.is_null() {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }
    let state = &*state;

    match message {
        WM_TRAY => match lparam.0 as u32 {
            WM_LBUTTONUP | WM_LBUTTONDBLCLK => show_window(state.main.0 as isize),
            WM_RBUTTONUP | WM_CONTEXTMENU => popup_menu(hwnd, state),
            _ => {}
        },
        WM_SHOW => show_window(state.main.0 as isize),
        WM_EXIT => (state.on_exit)(),
        // Explorer restarted: our icon is gone, add it again
        _ if message == state.taskbar_created && message != 0 => add_icon(hwnd, state),
        _ => return DefWindowProcW(hwnd, message, wparam, lparam),
    }
    LRESULT(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_truncated_and_terminated() {
        let mut buffer = [0xFFFFu16; 8];
        copy_text(&mut buffer, "æøå");
        assert_eq!(&buffer[..4], &[0xE6, 0xF8, 0xE5, 0]);

        copy_text(&mut buffer, "0123456789");
        assert_eq!(buffer[6], '6' as u16);
        assert_eq!(buffer[7], 0);
    }
}
