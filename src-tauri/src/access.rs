//! Getting Full Disk Access granted without a hunt through System Settings.
//!
//! `open` opens the Full Disk Access pane and floats a small panel beside its window, holding
//! Storage's own icon: drag it into the list and Storage is added. The panel is a native
//! non-activating `NSPanel`, so using it never brings Storage's main window over System Settings,
//! and the drag carries the app bundle's file URL, which is what the list accepts.
//!
//! While the panel is up a timer on the main run loop keeps it next to the System Settings window
//! (until the panel is moved by hand), shows it only while System Settings is the frontmost app,
//! and checks for access once a second. When access arrives the panel closes and the main window
//! is told (`full-disk-access`), so it can scan again; when System Settings quits or the panel is
//! closed, the helper just goes away.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAccessibility, NSBackingStoreType, NSBezierPath, NSColor, NSCursor, NSDragOperation, NSDraggingContext,
    NSDraggingItem, NSDraggingSession, NSDraggingSource, NSEvent, NSFont, NSImage, NSImageView, NSPanel,
    NSPasteboardWriting, NSRunningApplication, NSScreen, NSTextField, NSView, NSVisualEffectBlendingMode,
    NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindowButton, NSWindowCollectionBehavior,
    NSWindowStyleMask, NSWorkspace,
};
use objc2_core_graphics::{CGWindowListCopyWindowInfo, CGWindowListOption};
use objc2_foundation::{
    NSArray, NSDictionary, NSNumber, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSTimer, NSURL, ns_string,
};
use storage_core::sys;
use tauri::{AppHandle, Emitter};

/// The event the main window gets when access has been granted.
pub const GRANTED: &str = "full-disk-access";

const PANE: &str = "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

const WIDTH: f64 = 360.0;
const HEIGHT: f64 = 118.0;
const PAD: f64 = 12.0;
const TILE_H: f64 = 66.0;
const ICON: f64 = 42.0;
/// Space between the panel and the System Settings window.
const GAP: f64 = 10.0;
/// About how wide System Settings' sidebar is: the panel centres under the pane beside it.
const SIDEBAR: f64 = 230.0;

const TICK: f64 = 0.12;
const CHECK_EVERY: Duration = Duration::from_secs(1);
/// How long to wait for the System Settings window before showing the panel anywhere.
const FIND_FOR: Duration = Duration::from_secs(3);
/// The helper gives up after this, in case it was simply forgotten.
const GIVE_UP: Duration = Duration::from_secs(15 * 60);

/// Opens Full Disk Access in System Settings and, unless Storage has it already, the drag panel.
pub fn open(app: &AppHandle) -> Result<(), String> {
    Command::new("open").arg(PANE).spawn().map(drop).map_err(|e| e.to_string())?;
    if sys::full_disk_access() {
        return Ok(());
    }
    let handle = app.clone();
    app.run_on_main_thread(move || show(handle)).map_err(|e| e.to_string())
}

struct Helper {
    panel: Retained<NSPanel>,
    timer: Retained<NSTimer>,
    opened: Instant,
    checked: Instant,
    /// The System Settings window has been seen, so its absence now means it was closed.
    found: bool,
    /// Where the panel was put last. Anywhere else means it was moved by hand, and stays there.
    placed: Option<NSPoint>,
    follow: bool,
    /// Ordered in by the helper. Not visible while this is set means it was closed.
    shown: bool,
}

thread_local! {
    static HELPER: RefCell<Option<Helper>> = const { RefCell::new(None) };
}

fn show(app: AppHandle) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    HELPER.with_borrow_mut(|slot| {
        if let Some(h) = slot {
            // Asked again: back beside System Settings.
            h.follow = true;
            h.placed = None;
            h.opened = Instant::now();
            return;
        }
        let panel = panel(mtm, &bundle());
        let block = RcBlock::new(move |_: NonNull<NSTimer>| tick(&app));
        // SAFETY: the timer is scheduled on, and so only ever fires on, the main run loop.
        let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(TICK, true, &block) };
        let now = Instant::now();
        *slot = Some(Helper {
            panel,
            timer,
            opened: now,
            checked: now,
            found: false,
            placed: None,
            follow: true,
            shown: false,
        });
    });
}

fn close() {
    if let Some(h) = HELPER.with_borrow_mut(Option::take) {
        h.timer.invalidate();
        h.panel.orderOut(None);
    }
}

enum Outcome {
    Granted,
    Gone,
}

fn tick(app: &AppHandle) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let outcome = HELPER.with_borrow_mut(|slot| {
        let h = slot.as_mut()?;
        if h.shown && !h.panel.isVisible() {
            return Some(Outcome::Gone);
        }
        if h.checked.elapsed() >= CHECK_EVERY {
            h.checked = Instant::now();
            if sys::full_disk_access() {
                return Some(Outcome::Granted);
            }
        }
        if h.opened.elapsed() >= GIVE_UP {
            return Some(Outcome::Gone);
        }
        let running = settings_app();
        let window = running.as_ref().and_then(|a| settings_window(mtm, a.processIdentifier()));
        if window.is_some() {
            h.found = true;
        } else if running.is_none() && (h.found || h.opened.elapsed() >= FIND_FOR) {
            return Some(Outcome::Gone);
        }
        let frame = h.panel.frame();
        if h.placed.is_some_and(|p| (p.x - frame.origin.x).abs() > 1.0 || (p.y - frame.origin.y).abs() > 1.0) {
            h.follow = false;
            h.placed = Some(frame.origin);
        }
        if h.follow {
            let at = match window {
                Some(w) => Some(beside(mtm, w, frame.size)),
                None if h.placed.is_none() && h.opened.elapsed() >= FIND_FOR => fallback(mtm, frame.size),
                None => None,
            };
            if let Some(at) = at.filter(|&at| h.placed != Some(at)) {
                h.panel.setFrameOrigin(at);
                h.placed = Some(at);
            }
        }
        // Only while System Settings is in front: the panel shouldn't float over everything else.
        let want = h.placed.is_some() && settings_in_front();
        if want != h.shown {
            if want {
                h.panel.orderFrontRegardless();
            } else {
                h.panel.orderOut(None);
            }
            h.shown = want;
        }
        None
    });
    match outcome {
        Some(Outcome::Granted) => {
            close();
            let _ = app.emit(GRANTED, ());
        }
        Some(Outcome::Gone) => close(),
        None => {}
    }
}

/// The app to drag in: the bundle Storage runs from, or its executable when it isn't in one
/// (`cargo run`; macOS then holds the terminal responsible, so that's what needs access).
fn bundle() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    exe.ancestors().find(|p| p.extension().is_some_and(|e| e == "app")).map(Path::to_path_buf).unwrap_or(exe)
}

fn settings_id() -> &'static NSString {
    ns_string!("com.apple.systempreferences")
}

fn settings_app() -> Option<Retained<NSRunningApplication>> {
    NSRunningApplication::runningApplicationsWithBundleIdentifier(settings_id()).firstObject()
}

fn settings_in_front() -> bool {
    NSWorkspace::sharedWorkspace()
        .frontmostApplication()
        .and_then(|a| a.bundleIdentifier())
        .is_some_and(|id| &*id == settings_id())
}

/// System Settings' main window, in AppKit's coordinates (origin at the bottom left of the main
/// screen). Window bounds and owners can be read without Screen Recording access; titles can't.
fn settings_window(mtm: MainThreadMarker, pid: i32) -> Option<NSRect> {
    let list = CGWindowListCopyWindowInfo(
        CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements,
        0,
    )?;
    // SAFETY: CFArray is toll-free bridged with NSArray, and the window list holds dictionaries.
    let list: &NSArray<NSDictionary<NSString, AnyObject>> = unsafe { &*(&*list as *const _ as *const NSArray<_>) };
    let number = |d: &NSDictionary<NSString, AnyObject>, key: &NSString| {
        d.objectForKey(key).and_then(|v| v.downcast::<NSNumber>().ok()).map(|n| n.doubleValue())
    };
    let top = NSScreen::screens(mtm).firstObject()?.frame().size.height;
    list.iter()
        .filter(|w| number(w, ns_string!("kCGWindowOwnerPID")) == Some(pid as f64))
        .filter(|w| number(w, ns_string!("kCGWindowLayer")) == Some(0.0))
        .filter_map(|w| {
            let b = w.objectForKey(ns_string!("kCGWindowBounds"))?.downcast::<NSDictionary>().ok()?;
            // SAFETY: the bounds dictionary has string keys and number values.
            let b: &NSDictionary<NSString, AnyObject> = unsafe { &*(&*b as *const NSDictionary as *const _) };
            let (x, y) = (number(b, ns_string!("X"))?, number(b, ns_string!("Y"))?);
            let (w, h) = (number(b, ns_string!("Width"))?, number(b, ns_string!("Height"))?);
            (w > 200.0 && h > 200.0).then(|| NSRect::new(NSPoint::new(x, top - y - h), NSSize::new(w, h)))
        })
        .max_by(|a, b| (a.size.width * a.size.height).total_cmp(&(b.size.width * b.size.height)))
}

/// Where the panel goes for a System Settings window: under the pane with the list if there's room,
/// otherwise beside the window, otherwise over its bottom corner. Always on the window's screen.
fn beside(mtm: MainThreadMarker, w: NSRect, size: NSSize) -> NSPoint {
    let mid = NSPoint::new(w.origin.x + w.size.width / 2.0, w.origin.y + w.size.height / 2.0);
    let screens = NSScreen::screens(mtm);
    let screen = screens.iter().find(|s| contains(s.frame(), mid)).or_else(|| screens.firstObject());
    let Some(vf) = screen.map(|s| s.visibleFrame()) else { return w.origin };
    let (left, right, bottom) = (w.origin.x, w.origin.x + w.size.width, w.origin.y);
    let pane = left + SIDEBAR.min(w.size.width / 3.0);
    let at = if bottom - GAP - size.height >= vf.origin.y {
        NSPoint::new((pane + right) / 2.0 - size.width / 2.0, bottom - GAP - size.height)
    } else if right + GAP + size.width <= vf.origin.x + vf.size.width {
        NSPoint::new(right + GAP, bottom)
    } else if left - GAP - size.width >= vf.origin.x {
        NSPoint::new(left - GAP - size.width, bottom)
    } else {
        NSPoint::new(right - size.width - PAD, bottom + PAD)
    };
    clamp(at, size, vf)
}

/// No System Settings window to go by: low in the middle of the main screen.
fn fallback(mtm: MainThreadMarker, size: NSSize) -> Option<NSPoint> {
    let vf = NSScreen::mainScreen(mtm)?.visibleFrame();
    let at = NSPoint::new(vf.origin.x + (vf.size.width - size.width) / 2.0, vf.origin.y + vf.size.height / 5.0);
    Some(clamp(at, size, vf))
}

fn contains(r: NSRect, p: NSPoint) -> bool {
    p.x >= r.origin.x && p.x < r.origin.x + r.size.width && p.y >= r.origin.y && p.y < r.origin.y + r.size.height
}

fn clamp(at: NSPoint, size: NSSize, vf: NSRect) -> NSPoint {
    let x = at.x.min(vf.origin.x + vf.size.width - size.width).max(vf.origin.x);
    let y = at.y.min(vf.origin.y + vf.size.height - size.height).max(vf.origin.y);
    NSPoint::new(x.round(), y.round())
}

fn panel(mtm: MainThreadMarker, app: &Path) -> Retained<NSPanel> {
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(WIDTH, HEIGHT));
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::FullSizeContentView
        | NSWindowStyleMask::NonactivatingPanel;
    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        rect,
        style,
        NSBackingStoreType::Buffered,
        false,
    );
    // SAFETY: the panel is owned by a `Retained`, which releases it; AppKit mustn't as well.
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setFloatingPanel(true);
    panel.setHidesOnDeactivate(false);
    panel.setBecomesKeyOnlyIfNeeded(true);
    panel.setExcludedFromWindowsMenu(true);
    panel.setMovableByWindowBackground(true);
    panel.setTitlebarAppearsTransparent(true);
    panel.setTitle(ns_string!("Full Disk Access"));
    panel.setCollectionBehavior(
        NSWindowCollectionBehavior::MoveToActiveSpace | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );
    for b in [NSWindowButton::MiniaturizeButton, NSWindowButton::ZoomButton] {
        if let Some(b) = panel.standardWindowButton(b) {
            b.setHidden(true);
        }
    }

    let back = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), rect);
    back.setMaterial(NSVisualEffectMaterial::Popover);
    back.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    // Storage isn't the active app while this is up, and an inactive effect view goes flat grey.
    back.setState(NSVisualEffectState::Active);
    panel.setContentView(Some(&back));

    let tile = Tile::new(mtm, NSRect::new(NSPoint::new(PAD, PAD), NSSize::new(WIDTH - 2.0 * PAD, TILE_H)), app);
    back.addSubview(&tile);
    panel
}

struct TileIvars {
    url: Retained<NSURL>,
    icon: Retained<NSImage>,
    /// A drag has started for the current click.
    dragging: Cell<bool>,
}

define_class!(
    /// The app's icon, name and a hint, in a dashed box. Dragging anywhere in it drags the app.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "StorageAccessTile"]
    #[ivars = TileIvars]
    struct Tile;

    impl Tile {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(mouseDownCanMoveWindow))]
        fn mouse_down_can_move_window(&self) -> bool {
            false
        }

        /// The labels and image inside are only decoration: every click is the tile's.
        #[unsafe(method(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> *mut NSView {
            if contains(self.frame(), point) { self as *const Self as *mut NSView } else { std::ptr::null_mut() }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            self.ivars().dragging.set(false);
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            if !self.ivars().dragging.replace(true) {
                self.drag(event);
            }
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            self.addCursorRect_cursor(self.bounds(), &NSCursor::openHandCursor());
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let b = self.bounds();
            let r = NSRect::new(NSPoint::new(0.5, 0.5), NSSize::new(b.size.width - 1.0, b.size.height - 1.0));
            let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(r, 10.0, 10.0);
            NSColor::labelColor().colorWithAlphaComponent(0.05).setFill();
            path.fill();
            let dash = [5.0, 4.0];
            // SAFETY: the pattern holds the two lengths it's said to.
            unsafe { path.setLineDash_count_phase(dash.as_ptr(), 2, 0.0) };
            path.setLineWidth(1.0);
            NSColor::tertiaryLabelColor().setStroke();
            path.stroke();
        }
    }

    unsafe impl NSObjectProtocol for Tile {}

    unsafe impl NSDraggingSource for Tile {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn operation_mask(&self, _session: &NSDraggingSession, _context: NSDraggingContext) -> NSDragOperation {
            NSDragOperation::Copy | NSDragOperation::Generic
        }
    }
);

impl Tile {
    fn new(mtm: MainThreadMarker, frame: NSRect, app: &Path) -> Retained<Self> {
        let path = NSString::from_str(&app.to_string_lossy());
        let url = NSURL::fileURLWithPath(&path);
        let icon = NSWorkspace::sharedWorkspace().iconForFile(&path);
        icon.setSize(NSSize::new(ICON, ICON));
        let this = Self::alloc(mtm).set_ivars(TileIvars { url, icon: icon.clone(), dragging: Cell::new(false) });
        // SAFETY: `initWithFrame:` is NSView's designated initialiser.
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };

        let name = app.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let width = frame.size.width;
        let text_x = PAD + ICON + 10.0;
        let text_w = width - text_x - 46.0;

        let image = NSImageView::imageViewWithImage(&icon, mtm);
        image.setFrame(icon_frame());
        this.addSubview(&image);

        let title = NSTextField::labelWithString(&NSString::from_str(&name), mtm);
        title.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
        title.setFrame(NSRect::new(NSPoint::new(text_x, 11.0), NSSize::new(text_w, 17.0)));
        this.addSubview(&title);

        let hint = NSTextField::wrappingLabelWithString(
            ns_string!("Drag into the Full Disk Access list, or switch it on if it's already there."),
            mtm,
        );
        hint.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        hint.setTextColor(Some(&NSColor::secondaryLabelColor()));
        hint.setFrame(NSRect::new(NSPoint::new(text_x, 29.0), NSSize::new(text_w, 30.0)));
        this.addSubview(&hint);

        if let Some(glyph) = NSImage::imageWithSystemSymbolName_accessibilityDescription(ns_string!("hand.draw"), None)
        {
            let view = NSImageView::imageViewWithImage(&glyph, mtm);
            view.setContentTintColor(Some(&NSColor::tertiaryLabelColor()));
            view.setFrame(NSRect::new(NSPoint::new(width - 38.0, (TILE_H - 24.0) / 2.0), NSSize::new(24.0, 24.0)));
            this.addSubview(&view);
        }

        this.setAccessibilityElement(true);
        this.setAccessibilityLabel(Some(&NSString::from_str(&format!(
            "{name}: drag into the Full Disk Access list in System Settings"
        ))));
        this
    }

    fn drag(&self, event: &NSEvent) {
        let ivars = self.ivars();
        let writer = ProtocolObject::<dyn NSPasteboardWriting>::from_ref(&*ivars.url);
        let item = NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), writer);
        // SAFETY: an NSImage is what the dragging frame's contents may be.
        unsafe { item.setDraggingFrame_contents(icon_frame(), Some(ivars.icon.as_ref())) };
        let items = NSArray::from_retained_slice(&[item]);
        let session = self.beginDraggingSessionWithItems_event_source(&items, event, ProtocolObject::from_ref(self));
        session.setAnimatesToStartingPositionsOnCancelOrFail(true);
    }
}

fn icon_frame() -> NSRect {
    NSRect::new(NSPoint::new(PAD, (TILE_H - ICON) / 2.0), NSSize::new(ICON, ICON))
}
