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
}
