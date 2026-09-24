//! WindowServer cursor ownership for a rail that deliberately stays inactive.
//! This is a private macOS API: resolve it at runtime and fall back to AppKit
//! when unavailable. The override is limited to this process and restored on exit.
use std::{ffi::{c_char, c_void}, ptr};
use objc2_foundation::NSString;

type Connection = unsafe extern "C" fn() -> i32;
type Set = unsafe extern "C" fn(i32, i32, *const c_void, *const c_void) -> i32;
type Copy = unsafe extern "C" fn(i32, i32, *const c_void, *mut *const c_void) -> i32;
extern "C" { fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void; }
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" { static kCFBooleanTrue: *const c_void; static kCFBooleanFalse: *const c_void; fn CFRelease(value: *const c_void); }

pub struct BackgroundCursor {
    api: Option<(Connection, Set, Copy)>,
    previous: Option<*const c_void>,
}
impl BackgroundCursor {
    pub fn new() -> Self {
        // RTLD_DEFAULT looks in already loaded frameworks; no hard link to
        // private symbols, so a future macOS change cannot prevent app startup.
        let api = unsafe {
            let handle = -2isize as *mut c_void;
            let main = dlsym(handle, c"CGSMainConnectionID".as_ptr());
            let set = dlsym(handle, c"CGSSetConnectionProperty".as_ptr());
            let copy = dlsym(handle, c"CGSCopyConnectionProperty".as_ptr());
            if main.is_null() || set.is_null() || copy.is_null() { None }
            else { Some((std::mem::transmute::<*mut c_void, Connection>(main), std::mem::transmute::<*mut c_void, Set>(set), std::mem::transmute::<*mut c_void, Copy>(copy))) }
        };
        Self { api, previous: None }
    }
    pub fn acquire(&mut self) -> bool {
        if self.previous.is_some() { return false; }
        let Some((main, set, copy)) = self.api else { return false; };
        let key = NSString::from_str("SetsCursorInBackground");
        let key = (&*key as *const NSString).cast();
        unsafe {
            let id = main();
            let mut previous = ptr::null();
            if copy(id, id, key, &mut previous) != 0 { return false; }
            if set(id, id, key, kCFBooleanTrue) == 0 { self.previous = Some(previous); return true; }
            else if !previous.is_null() { CFRelease(previous); }
        }
        false
    }
    pub fn release(&mut self) {
        let Some(previous) = self.previous.take() else { return; };
        let Some((main, set, _)) = self.api else { return; };
        let key = NSString::from_str("SetsCursorInBackground");
        unsafe {
            let id = main();
            set(id, id, (&*key as *const NSString).cast(), if previous.is_null() { kCFBooleanFalse } else { previous });
            if !previous.is_null() { CFRelease(previous); }
        }
    }
}
impl Drop for BackgroundCursor { fn drop(&mut self) { self.release(); } }
