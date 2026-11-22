//! Progress on the Windows taskbar button (ITaskbarList3): the green bar that fills
//! while the setup installs, red on error. A no-op on other platforms.
//!
//! The COM interface is declared by hand (a few vtable slots) instead of pulling in
//! the `windows` crate, which keeps OUTNO and every generated setup small.

use raw_window_handle::{HasWindowHandle, RawWindowHandle};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum State {
    /// No progress shown on the taskbar button.
    None,
    /// Pulsing green bar (amount unknown yet).
    Indeterminate,
    /// Green bar, 0.0 ..= 1.0
    Normal(f32),
    /// Red bar, 0.0 ..= 1.0
    Error(f32),
}

impl State {
    /// Round fractions so we only talk to the shell when the bar visibly changes.
    fn quantized(self) -> Self {
        let q = |f: f32| (f.clamp(0.0, 1.0) * 1000.0).round() / 1000.0;
        match self {
            State::Normal(f) => State::Normal(q(f)),
            State::Error(f) => State::Error(q(f)),
            s => s,
        }
    }
}

#[derive(Default)]
pub struct Taskbar {
    last: Option<State>,
    #[cfg(windows)]
    list: Option<win::TaskbarList>,
    #[cfg(windows)]
    unavailable: bool,
}

impl Taskbar {
    /// Call from `App::update` (UI thread). Cheap when the state hasn't changed.
    pub fn set(&mut self, frame: &eframe::Frame, state: State) {
        let state = state.quantized();
        if self.last == Some(state) {
            return;
        }
        let Some(hwnd) = hwnd(frame) else { return };
        if self.apply(hwnd, state) {
            self.last = Some(state);
        }
        // On failure (e.g. the taskbar button isn't created yet) `last` stays
        // unchanged, so the next frame retries.
    }

    #[cfg(windows)]
    fn apply(&mut self, hwnd: isize, state: State) -> bool {
        if self.unavailable {
            return false;
        }
        if self.list.is_none() {
            self.list = win::TaskbarList::new();
            if self.list.is_none() {
                self.unavailable = true; // e.g. no Explorer shell running
                return false;
            }
        }
        self.list.as_ref().is_some_and(|l| l.apply(hwnd, state))
    }

    #[cfg(not(windows))]
    fn apply(&mut self, _hwnd: isize, _state: State) -> bool {
        true
    }
}

fn hwnd(frame: &eframe::Frame) -> Option<isize> {
    match frame.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

#[cfg(windows)]
mod win {
    use super::State;
    use std::ffi::c_void;
    use std::ptr::null_mut;

    type Hresult = i32;
    type Hwnd = isize;

    #[repr(C)]
    struct Guid(u32, u16, u16, [u8; 8]);

    // {56FDF344-FD6D-11d0-958A-006097C9A090}
    const CLSID_TASKBAR_LIST: Guid = Guid(0x56FD_F344, 0xFD6D, 0x11D0, [0x95, 0x8A, 0x00, 0x60, 0x97, 0xC9, 0xA0, 0x90]);
    // {EA1AFB91-9E28-4B86-90E9-9E9F8A5EEFAF}
    const IID_ITASKBAR_LIST3: Guid = Guid(0xEA1A_FB91, 0x9E28, 0x4B86, [0x90, 0xE9, 0x9E, 0x9F, 0x8A, 0x5E, 0xEF, 0xAF]);

    const COINIT_APARTMENTTHREADED: u32 = 0x2;
    const CLSCTX_INPROC_SERVER: u32 = 0x1;

    const TBPF_NOPROGRESS: u32 = 0x0;
    const TBPF_INDETERMINATE: u32 = 0x1;
    const TBPF_NORMAL: u32 = 0x2;
    const TBPF_ERROR: u32 = 0x4;

    /// ITaskbarList3 vtable, in declaration order (IUnknown → ITaskbarList → 2 → 3).
    /// Only the slots we call are typed; the rest are placeholders.
    #[repr(C)]
    struct Vtbl {
        _query_interface: usize,
        _add_ref: usize,
        release: unsafe extern "system" fn(*mut c_void) -> u32,
        hr_init: unsafe extern "system" fn(*mut c_void) -> Hresult,
        _add_tab: usize,
        _delete_tab: usize,
        _activate_tab: usize,
        _set_active_alt: usize,
        _mark_fullscreen_window: usize,
        set_progress_value: unsafe extern "system" fn(*mut c_void, Hwnd, u64, u64) -> Hresult,
        set_progress_state: unsafe extern "system" fn(*mut c_void, Hwnd, u32) -> Hresult,
    }

    #[link(name = "ole32")]
    extern "system" {
        fn CoInitializeEx(reserved: *mut c_void, coinit: u32) -> Hresult;
        fn CoCreateInstance(
            clsid: *const Guid,
            outer: *mut c_void,
            context: u32,
            iid: *const Guid,
            out: *mut *mut c_void,
        ) -> Hresult;
    }

    pub struct TaskbarList(*mut c_void);

    impl TaskbarList {
        pub fn new() -> Option<Self> {
            unsafe {
                // winit has normally initialised OLE on the UI thread already; then this
                // just returns S_FALSE. Either way the result doesn't matter here.
                CoInitializeEx(null_mut(), COINIT_APARTMENTTHREADED);
                let mut p: *mut c_void = null_mut();
                let hr = CoCreateInstance(&CLSID_TASKBAR_LIST, null_mut(), CLSCTX_INPROC_SERVER, &IID_ITASKBAR_LIST3, &mut p);
                if hr < 0 || p.is_null() {
                    return None;
                }
                let list = TaskbarList(p);
                if (list.vtbl().hr_init)(list.0) < 0 {
                    return None; // dropped → Release
                }
                Some(list)
            }
        }

        fn vtbl(&self) -> &Vtbl {
            unsafe { &**(self.0 as *const *const Vtbl) }
        }

        pub fn apply(&self, hwnd: Hwnd, state: State) -> bool {
            const SCALE: u64 = 1000;
            let v = self.vtbl();
            unsafe {
                let (flag, value) = match state {
                    State::None => (TBPF_NOPROGRESS, None),
                    State::Indeterminate => (TBPF_INDETERMINATE, None),
                    State::Normal(f) => (TBPF_NORMAL, Some(f)),
                    State::Error(f) => (TBPF_ERROR, Some(f)),
                };
                // Set the value first: it implicitly switches to NORMAL, then the state wins.
                if let Some(f) = value {
                    if (v.set_progress_value)(self.0, hwnd, (f * SCALE as f32) as u64, SCALE) < 0 {
                        return false;
                    }
                }
                (v.set_progress_state)(self.0, hwnd, flag) >= 0
            }
        }
    }

    impl Drop for TaskbarList {
        fn drop(&mut self) {
            unsafe {
                (self.vtbl().release)(self.0);
            }
        }
    }
}
