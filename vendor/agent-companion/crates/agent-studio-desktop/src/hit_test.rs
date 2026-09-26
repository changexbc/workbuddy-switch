use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Region {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    #[serde(default)]
    pub cursor: Option<String>,
}
impl Region {
    pub fn valid(&self) -> bool {
        [self.x, self.y, self.width, self.height]
            .iter()
            .all(|v| v.is_finite())
            && self.width > 0.0
            && self.height > 0.0
    }
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x && y >= self.y && x <= self.x + self.width && y <= self.y + self.height
    }
}
pub type Regions = Arc<Mutex<Vec<Region>>>;

/// Debounce for the platform "window ignores the mouse" flag. The Windows
/// poller feeds it the desired value every tick and only touches the window
/// when the answer flips. `Default` is "nothing written yet".
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
#[derive(Default)]
struct IgnoreState {
    last: Option<bool>,
}
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
impl IgnoreState {
    /// `Some(desired)` means the window needs the update; `None` means the
    /// current value is already right.
    fn update(&mut self, desired: bool) -> Option<bool> {
        if self.last == Some(desired) {
            return None;
        }
        self.last = Some(desired);
        Some(desired)
    }
    /// A failed write must not stick: the next tick should try the same value
    /// again instead of treating the switch as already applied.
    fn forget(&mut self) {
        self.last = None;
    }
}

/// The cursor and the window origin are physical pixels; regions come from the
/// DOM's `getBoundingClientRect` in CSS pixels. Dividing the offset by the
/// window's scale factor puts the cursor in the same space as the regions,
/// including on secondary monitors whose virtual-screen origin is negative.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn logical_point(cursor: (f64, f64), origin: (i32, i32), scale: f64) -> (f64, f64) {
    (
        (cursor.0 - origin.0 as f64) / scale,
        (cursor.1 - origin.1 as f64) / scale,
    )
}

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use block2::RcBlock;
    use crate::background_cursor::BackgroundCursor;
    use objc2::{rc::Retained, runtime::AnyObject};
    use objc2_app_kit::{NSCursor, NSEvent, NSEventMask, NSWindow};
    use std::{cell::RefCell, ptr::NonNull, rc::Rc};
    thread_local! { static MONITORS: RefCell<Vec<Retained<AnyObject>>> = const { RefCell::new(Vec::new()) }; }

    thread_local! {
        static BACKGROUND_CURSOR: RefCell<BackgroundCursor> = RefCell::new(BackgroundCursor::new());
        static OWNS_CURSOR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    fn release_cursor() {
        if OWNS_CURSOR.replace(false) { NSCursor::arrowCursor().set(); }
        BACKGROUND_CURSOR.with(|cursor| cursor.borrow_mut().release());
    }

    // Both handlers execute on AppKit's main thread. The rail is retained until
    // application exit; its close action hides it instead of destroying NSWindow.
    pub fn refresh(pointer: usize, regions: &Regions) {
        let window = unsafe { &*(pointer as *const NSWindow) };
        if !window.isVisible() { release_cursor(); return; }
        if NSEvent::pressedMouseButtons() != 0 { return; }
        let frame = window.frame();
        let cursor = NSEvent::mouseLocation();
        let x = cursor.x - frame.origin.x;
        let y = frame.origin.y + frame.size.height - cursor.y;
        let regions = regions.lock().unwrap();
        let region = regions.iter().find(|r| r.contains(x, y));
        let hit = region.is_some();
        match region.and_then(|r| r.cursor.as_deref()) {
            Some("pointer" | "grab") => {
                let acquired = BACKGROUND_CURSOR.with(|cursor| cursor.borrow_mut().acquire());
                // AppKit caches the last cursor even when WindowServer ignored
                // it while inactive. Force a transition after taking ownership.
                if acquired { NSCursor::arrowCursor().set(); }
                OWNS_CURSOR.set(true);
                if region.unwrap().cursor.as_deref() == Some("grab") { NSCursor::openHandCursor().set(); }
                else { NSCursor::pointingHandCursor().set(); }
            }
            _ => release_cursor(),
        }
        window.setAcceptsMouseMovedEvents(true);
        if window.ignoresMouseEvents() == hit {
            window.setIgnoresMouseEvents(!hit);
        }
    }
    pub fn install(pointer: usize, regions: Regions, on_pointer: impl Fn(Option<(f64, f64)>) + 'static) {
        // A non-focusable WKWebView does not reliably receive DOM hover events.
        // Reuse the AppKit event monitors to forward coordinates without focusing
        // the window, synthesizing clicks, or adding an idle polling loop.
        let last = RefCell::new(None);
        let hover_regions = regions.clone();
        let report = Rc::new(move || {
            let window = unsafe { &*(pointer as *const NSWindow) };
            if NSEvent::pressedMouseButtons() != 0 { return; }
            let frame = window.frame();
            let cursor = NSEvent::mouseLocation();
            let point = (cursor.x - frame.origin.x, frame.origin.y + frame.size.height - cursor.y);
            let current = if window.isVisible() && hover_regions.lock().unwrap().iter().any(|r| r.contains(point.0, point.1)) { Some(point) } else { None };
            if *last.borrow() != current {
                *last.borrow_mut() = current;
                on_pointer(current);
            }
        });
        let mask = NSEventMask::MouseMoved | NSEventMask::LeftMouseUp | NSEventMask::RightMouseUp;
        let global_regions = regions.clone();
        let global_report = report.clone();
        let global = RcBlock::new(move |_: NonNull<NSEvent>| { refresh(pointer, &global_regions); global_report(); });
        let local_regions = regions.clone();
        let local = RcBlock::new(move |event: NonNull<NSEvent>| {
            refresh(pointer, &local_regions);
            report();
            event.as_ptr()
        });
        MONITORS.with(|tokens| {
            let mut tokens = tokens.borrow_mut();
            if let Some(token) =
                NSEvent::addGlobalMonitorForEventsMatchingMask_handler(mask, &global)
            {
                tokens.push(token);
            }
            if let Some(token) =
                unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(mask, &local) }
            {
                tokens.push(token);
            }
        });
        refresh(pointer, &regions);
    }
    pub fn remove() {
        release_cursor();
        MONITORS.with(|tokens| {
            for token in tokens.borrow_mut().drain(..) {
                unsafe { NSEvent::removeMonitor(&token) };
            }
        });
    }
}

#[cfg(target_os = "macos")]
pub use mac::{install, refresh, remove};

/// Windows has no per-pixel alpha on the rail, so without help the whole
/// 368x600 rectangle is a hit target and swallows clicks, wheel events and
/// hover from whatever is underneath. Tauri's whole-window
/// `set_ignore_cursor_events` is the only available switch (tao maps it to
/// `WS_EX_TRANSPARENT | WS_EX_LAYERED`), and the answer depends on where the
/// cursor is, so a thread re-evaluates it on a timer.
#[cfg(target_os = "windows")]
mod win {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, RecvTimeoutError, Sender};
    use std::time::Duration;
    use tauri::WebviewWindow;
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON, VK_XBUTTON1, VK_XBUTTON2,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

    /// Upper bound on how long the rail can keep swallowing clicks after the
    /// cursor leaves every region. The only knob meant for real-machine tuning.
    const POLL_INTERVAL: Duration = Duration::from_millis(40);

    struct Registration {
        wake: Sender<()>,
        stop: Arc<AtomicBool>,
    }

    /// Only the wake channel and the stop flag are shared. The window and the
    /// region list stay in the thread, and the ignore switch is written by that
    /// thread alone: `refresh` and `remove` only post signals, so two writers
    /// can never race the toggle.
    static ACTIVE: Mutex<Option<Registration>> = Mutex::new(None);

    fn active() -> std::sync::MutexGuard<'static, Option<Registration>> {
        // The polling thread never takes this lock, so poisoning would only
        // ever come from a panic outside it; stay functional anyway.
        ACTIVE.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub fn install(window: WebviewWindow, regions: Regions) {
        // Idempotent: dropping the previous registration closes its wake
        // channel, which makes the old thread quit on its next wait. It must
        // not run its exit write, or it could undo this thread's first tick.
        drop(active().take());
        let (wake, inbox) = mpsc::channel::<()>();
        let stop = Arc::new(AtomicBool::new(false));
        *active() = Some(Registration {
            wake,
            stop: stop.clone(),
        });
        std::thread::spawn(move || poll(window, regions, stop, inbox));
    }

    /// Wake the thread so a `set_hit_regions` update is evaluated right away
    /// instead of after the remaining sleep.
    pub fn refresh() {
        if let Some(registration) = active().as_ref() {
            let _ = registration.wake.send(());
        }
    }

    /// Ask the thread to reset the switch and exit. Deliberately no `join`:
    /// this runs on the main thread during `RunEvent::Exit`, and the thread may
    /// be waiting on a getter round-trip that only the main thread can answer.
    pub fn remove() {
        if let Some(registration) = active().as_ref() {
            registration.stop.store(true, Ordering::Relaxed);
            let _ = registration.wake.send(());
        }
    }

    fn poll(
        window: WebviewWindow,
        regions: Regions,
        stop: Arc<AtomicBool>,
        inbox: mpsc::Receiver<()>,
    ) {
        let mut ignore = IgnoreState::default();
        loop {
            if stop.load(Ordering::Relaxed) {
                // Best effort so an exiting app does not leave the window
                // transparent to the mouse. Losing the race is harmless: the
                // style bits die with the window.
                let _ = window.set_ignore_cursor_events(false);
                break;
            }
            tick(&window, &regions, &mut ignore);
            match inbox.recv_timeout(POLL_INTERVAL) {
                Ok(()) => {}
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    }

    fn tick(window: &WebviewWindow, regions: &Regions, ignore: &mut IgnoreState) {
        // 1. While a mouse button is held, Windows runs a modal move loop (this
        // is how the drag handle works) and its state does not tolerate style
        // changes. Skipping also avoids queueing getter round-trips behind it.
        if mouse_button_down() {
            return;
        }
        // 2. A hidden window cannot be hit. Skipping keeps
        // `WS_EX_LAYERED` from being toggled for nothing.
        if !window.is_visible().unwrap_or(false) {
            return;
        }
        // 3. Both sides are fetched fresh, so monitor and scale changes need no
        // invalidation logic. A failing getter just skips the tick.
        let Some(cursor) = cursor_position() else { return };
        let Ok(origin) = window.inner_position() else { return };
        let Ok(scale) = window.scale_factor() else { return };
        let (x, y) = logical_point(cursor, (origin.x, origin.y), scale);
        // 4. Hold the lock only for the scan, never across a window call.
        let hit = match regions.lock() {
            Ok(guard) => guard.iter().any(|region| region.contains(x, y)),
            Err(_) => return, // Poisoned: skip the tick rather than panic.
        };
        // 5. Write only when the answer flips. If the call fails, forget the
        // recorded value so a later tick retries instead of stalling R7.
        if let Some(desired) = ignore.update(!hit) {
            if window.set_ignore_cursor_events(desired).is_err() {
                ignore.forget();
            }
        }
    }

    fn mouse_button_down() -> bool {
        [
            VK_LBUTTON, VK_RBUTTON, VK_MBUTTON, VK_XBUTTON1, VK_XBUTTON2,
        ]
        .iter()
        .any(|key| (unsafe { GetAsyncKeyState(*key as i32) } as u16 & 0x8000) != 0)
    }

    fn cursor_position() -> Option<(f64, f64)> {
        let mut point = POINT { x: 0, y: 0 };
        if unsafe { GetCursorPos(&mut point) } == 0 {
            return None;
        }
        Some((point.x as f64, point.y as f64))
    }
}

#[cfg(target_os = "windows")]
pub use win::{install, refresh, remove};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transparent_gap_does_not_hit_the_card_or_rail() {
        let regions = [
            Region {
                x: 257.0,
                y: 10.0,
                width: 52.0,
                height: 200.0, cursor: None,
            },
            Region {
                x: 39.0,
                y: 45.0,
                width: 208.0,
                height: 140.0, cursor: None,
            },
        ];
        assert!(regions.iter().any(|r| r.contains(275.0, 30.0)));
        assert!(regions.iter().any(|r| r.contains(100.0, 90.0)));
        assert!(!regions.iter().any(|r| r.contains(100.0, 300.0)));
        assert!(!regions.iter().any(|r| r.contains(252.0, 30.0)));
    }
    #[test]
    fn invalid_coordinates_are_rejected() {
        assert!(!Region {
            x: f64::NAN,
            y: 0.0,
            width: 10.0,
            height: 10.0, cursor: None
        }
        .valid());
        assert!(!Region {
            x: 0.0,
            y: 0.0,
            width: -1.0,
            height: 10.0, cursor: None
        }
        .valid());
    }
    #[test]
    fn ignore_state_writes_the_first_value_and_then_only_changes() {
        let mut state = IgnoreState::default();
        assert_eq!(state.update(true), Some(true));
        assert_eq!(state.update(true), None);
        assert_eq!(state.update(false), Some(false));
        assert_eq!(state.update(false), None);
        assert_eq!(state.update(true), Some(true));
        state.forget();
        assert_eq!(state.update(true), Some(true));
    }
    #[test]
    fn logical_point_divides_the_offset_by_the_scale_factor() {
        assert_eq!(logical_point((110.0, 120.0), (100, 100), 1.0), (10.0, 20.0));
        assert_eq!(logical_point((225.0, 325.0), (100, 200), 1.25), (100.0, 100.0));
        assert_eq!(logical_point((300.0, 500.0), (100, 100), 2.0), (100.0, 200.0));
    }
    #[test]
    fn logical_point_handles_negative_virtual_screen_coordinates() {
        // A secondary monitor left of / above the primary puts both the window
        // origin and the cursor at negative virtual-screen coordinates.
        assert_eq!(logical_point((-1800.0, -950.0), (-1920, -1080), 1.0), (120.0, 130.0));
        assert_eq!(logical_point((-1840.0, -1000.0), (-1920, -1080), 2.0), (40.0, 40.0));
    }
}
