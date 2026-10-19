//! Offscreen probe (temporary): does wry's `set_bounds` move/resize an X11
//! child window after creation, and does WebKit's layout follow?
//!
//! Nothing is ever mapped to the screen: the parent GTK window is realized and
//! never shown, so the child X window wry parents into it is unviewable.

#[cfg(all(target_os = "linux", feature = "webview"))]
mod probe {
    use std::sync::{Arc, Mutex};

    use gtk::glib::translate::ToGlibPtr;
    use gtk::prelude::*;
    use wry::dpi::{PhysicalPosition, PhysicalSize};
    use wry::raw_window_handle::{
        HandleError, HasWindowHandle, RawWindowHandle, WindowHandle, XlibWindowHandle,
    };
    use wry::{Rect, WebView, WebViewBuilder, WebViewExtUnix};

    unsafe extern "C" {
        fn gdk_x11_window_get_xid(w: *mut gtk::gdk::ffi::GdkWindow) -> std::os::raw::c_ulong;
    }

    struct Parent(std::os::raw::c_ulong);

    impl HasWindowHandle for Parent {
        fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
            let handle = XlibWindowHandle::new(self.0);
            Ok(unsafe { WindowHandle::borrow_raw(RawWindowHandle::Xlib(handle)) })
        }
    }

    fn pump(ms: u64) {
        let until = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        while std::time::Instant::now() < until {
            while gtk::events_pending() {
                gtk::main_iteration_do(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect {
            position: PhysicalPosition::new(x, y).into(),
            size: PhysicalSize::new(w, h).into(),
        }
    }

    /// The X server's own answer for the container window, plus what GTK
    /// thinks the webview widget and wry's internal toplevel were given.
    fn report(tag: &str, view: &WebView, viewport: &Arc<Mutex<Option<String>>>) {
        let b = view.bounds().expect("XGetWindowAttributes");
        let (bx, by): (i32, i32) = b.position.to_logical::<i32>(1.0).into();
        let (bw, bh): (i32, i32) = b.size.to_logical::<i32>(1.0).into();

        let webview = view.webview();
        let a = webview.allocation();
        let top = webview.toplevel();
        let (tw, th) = top
            .as_ref()
            .map(|t| {
                let a = t.allocation();
                (a.width(), a.height())
            })
            .unwrap_or((-1, -1));
        let (gw, gh) = top
            .as_ref()
            .and_then(|t| t.window())
            .map(|w| (w.width(), w.height()))
            .unwrap_or((-1, -1));

        *viewport.lock().unwrap() = None;
        let sink = viewport.clone();
        let _ = view.evaluate_script_with_callback(
            "JSON.stringify([innerWidth, innerHeight])",
            move |r| {
                *sink.lock().unwrap() = Some(r);
            },
        );
        for _ in 0..200 {
            if viewport.lock().unwrap().is_some() {
                break;
            }
            pump(10);
        }
        let page = viewport
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| "<no answer>".into());

        println!(
            "{tag:<26} X child = {bw}x{bh} @ {bx},{by} | wry gtk toplevel alloc = {tw}x{th}, \
             gdk = {gw}x{gh} | webkit widget alloc = {}x{} @ {},{} | page innerW/H = {page}",
            a.width(),
            a.height(),
            a.x(),
            a.y(),
        );
    }

    pub fn run() {
        assert!(gtk::init().is_ok(), "gtk");

        let parent = gtk::Window::new(gtk::WindowType::Toplevel);
        parent.set_default_size(1600, 1000);
        // Realized, never shown: the X window exists, nothing is on screen.
        parent.realize();
        let gdk_window = parent.window().expect("a realized parent");
        let ptr: *mut gtk::gdk::ffi::GdkWindow = gdk_window.to_glib_none().0;
        let xid = unsafe { gdk_x11_window_get_xid(ptr) };
        println!("parent X window = 0x{xid:x} (realized, unmapped)\n");

        let a = rect(12.0, 104.0, 900.0, 600.0);
        let b = rect(300.0, 150.0, 1400.0, 820.0);
        let c = rect(12.0, 104.0, 640.0, 480.0);

        let view = WebViewBuilder::new()
            .with_html("<html><body><p>probe</p></body></html>")
            .with_bounds(a)
            .build_as_child(&Parent(xid))
            .expect("a child webview");

        let viewport = Arc::new(Mutex::new(None));

        pump(800);
        report("A at creation", &view, &viewport);

        let _ = view.set_bounds(b);
        pump(300);
        report("B right after set_bounds", &view, &viewport);
        pump(800);
        report("B settled", &view, &viewport);

        // "Back to the list, then open another message" as Post does it: the
        // pane is hidden and shown again, and the `applied` gate then says the
        // rectangle is unchanged, so NO set_bounds follows.
        let _ = view.set_visible(false);
        pump(300);
        let _ = view.set_visible(true);
        pump(300);
        report("hide/show, no set_bounds", &view, &viewport);
        pump(800);
        report("  ... settled", &view, &viewport);

        // And with the placement the gate skipped.
        let _ = view.set_bounds(b);
        pump(500);
        report("hide/show + set_bounds(B)", &view, &viewport);

        // A live drag: many placements back to back, with GTK stepped between
        // them the way Post's frame loop steps it.
        for i in 0..40 {
            let w = 1400.0 - (i as f64) * 15.0;
            let h = 820.0 - (i as f64) * 7.0;
            let _ = view.set_bounds(rect(300.0, 150.0, w, h));
            while gtk::events_pending() {
                gtk::main_iteration_do(false);
            }
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
        pump(600);
        report("after a 40-step drag", &view, &viewport);

        let _ = view.set_bounds(c);
        pump(600);
        report("C", &view, &viewport);

        // The app's own window is dragged bigger, then the pane is placed.
        parent.resize(2400, 1400);
        pump(400);
        report("parent grew, no place", &view, &viewport);
        let _ = view.set_bounds(b);
        pump(600);
        report("parent grew, placed B", &view, &viewport);

        // Another message into the same pane.
        let _ = view.load_html("<p>another message</p><img src='data:image/gif;base64,\
                                R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7'>");
        pump(800);
        report("load_html, no place", &view, &viewport);
        let _ = view.set_bounds(c);
        pump(600);
        report("load_html, placed C", &view, &viewport);

        drop(view);
        pump(100);
    }
}

#[cfg(all(target_os = "linux", feature = "webview"))]
fn main() {
    probe::run();
}

#[cfg(not(all(target_os = "linux", feature = "webview")))]
fn main() {}
