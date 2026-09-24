//! Run explicitly on macOS: briefly sets the native cursor and restores it.
#[cfg(target_os = "macos")]
#[path = "../src/background_cursor.rs"]
mod background_cursor;
#[cfg(target_os = "macos")]
fn main() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSCursor};
    let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
    assert!(!app.isActive(), "Probe must remain a background application");
    let hand = NSCursor::pointingHandCursor();
    hand.set();
    std::thread::sleep(std::time::Duration::from_millis(150));
    #[allow(deprecated)]
    let before = NSCursor::currentSystemCursor().unwrap().image().TIFFRepresentation().unwrap().to_vec();
    let mut owner = background_cursor::BackgroundCursor::new();
    let acquired = owner.acquire();
    NSCursor::arrowCursor().set();
    hand.set();
    std::thread::sleep(std::time::Duration::from_millis(150));
    #[allow(deprecated)]
    let after = NSCursor::currentSystemCursor().unwrap().image().TIFFRepresentation().unwrap().to_vec();
    let expected = hand.image().TIFFRepresentation().unwrap().to_vec();
    NSCursor::arrowCursor().set();
    owner.release();
    println!("background={} acquired={} before_hand={} after_hand={}", !app.isActive(), acquired, before == expected, after == expected);
    assert!(acquired);
    assert_eq!(after, expected, "System cursor must actually become the native pointing hand");
}
#[cfg(not(target_os = "macos"))]
fn main() {}
