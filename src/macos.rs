//! Native menus supplement winit's existing application delegate. Keep that
//! delegate in place: winit owns the event loop and its lifecycle callbacks.
use anyhow::{Context, Result, ensure};
use objc2::{
    AnyThread, MainThreadMarker, MainThreadOnly,
    rc::Retained,
    runtime::{AnyObject, Imp, Sel},
    sel,
};
use objc2_app_kit::{NSApplication, NSImage, NSMenu, NSMenuItem};
use objc2_foundation::{NSData, ns_string};
use std::{
    cell::RefCell,
    sync::{Mutex, OnceLock},
};

thread_local! {
    static DOCK_MENU: RefCell<Option<Retained<NSMenu>>> = const { RefCell::new(None) };
}
static CONTEXT: OnceLock<egui::Context> = OnceLock::new();
static NEW_WINDOW: OnceLock<Box<dyn Fn() -> Result<()> + Send + Sync>> = OnceLock::new();
static ERROR: Mutex<Option<String>> = Mutex::new(None);

extern "C-unwind" fn dock_menu(_: &AnyObject, _: Sel, _: &NSApplication) -> *mut NSMenu {
    // AppKit borrows this menu; the thread-local owner keeps it alive.
    DOCK_MENU.with(|menu| {
        menu.borrow().as_ref().map_or(std::ptr::null_mut(), |menu| {
            Retained::as_ptr(menu).cast_mut()
        })
    })
}

extern "C-unwind" fn new_window(_: &AnyObject, _: Sel, _: *mut AnyObject) {
    // Run from the native action so this also works while the terminal window
    // is minimized and egui is not repainting.
    if let Some(open) = NEW_WINDOW.get()
        && let Err(error) = open()
    {
        *ERROR.lock().unwrap() = Some(format!("Cannot open a new window: {error}"));
        if let Some(ctx) = CONTEXT.get() {
            ctx.request_repaint();
        }
    }
}

pub fn take_error() -> Option<String> {
    ERROR.lock().unwrap().take()
}

fn new_window_item(mtm: MainThreadMarker, target: &AnyObject) -> Retained<NSMenuItem> {
    // SAFETY: the target's class has slermNewWindow: installed below, with the
    // standard void(id) action signature. The application owns the delegate.
    unsafe {
        let item = NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            ns_string!("New Window"),
            Some(sel!(slermNewWindow:)),
            // Native key equivalents intercept terminal/configured shortcuts.
            ns_string!(""),
        );
        item.setTarget(Some(target));
        item
    }
}

pub fn install(
    ctx: &egui::Context,
    open: impl Fn() -> Result<()> + Send + Sync + 'static,
) -> Result<()> {
    let mtm = MainThreadMarker::new().context("macOS menus require the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    let delegate = app
        .delegate()
        .context("Application delegate is not ready")?;
    let target: &AnyObject = delegate.as_ref();
    let class = target.class();
    ensure!(
        class.instance_method(sel!(applicationDockMenu:)).is_none(),
        "Application already has a Dock menu"
    );
    ensure!(
        class.instance_method(sel!(slermNewWindow:)).is_none(),
        "Window action is already installed"
    );
    // SAFETY: Add only previously absent methods, without changing the
    // delegate's class, ivars, or winit's methods. Both implementations match
    // the Objective-C encodings: id(id) and void(id), plus self and _cmd.
    unsafe {
        let class = (class as *const objc2::runtime::AnyClass).cast_mut();
        let dock_imp: Imp = std::mem::transmute(
            dock_menu as extern "C-unwind" fn(&AnyObject, Sel, &NSApplication) -> *mut NSMenu,
        );
        let action_imp: Imp = std::mem::transmute(
            new_window as extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject),
        );
        ensure!(
            objc2::ffi::class_addMethod(
                class,
                sel!(applicationDockMenu:),
                dock_imp,
                c"@@:@".as_ptr()
            )
            .as_bool(),
            "Cannot install Dock menu"
        );
        ensure!(
            objc2::ffi::class_addMethod(class, sel!(slermNewWindow:), action_imp, c"v@:@".as_ptr())
                .as_bool(),
            "Cannot install New Window action"
        );
    }
    let _ = CONTEXT.set(ctx.clone());
    let _ = NEW_WINDOW.set(Box::new(open));
    let menu = NSMenu::new(mtm);
    menu.addItem(&new_window_item(mtm, target));
    DOCK_MENU.with(|slot| *slot.borrow_mut() = Some(menu));

    let menubar = app.mainMenu().context("Application menu is not ready")?;
    let file = NSMenuItem::new(mtm);
    file.setTitle(ns_string!("File"));
    let file_menu = NSMenu::new(mtm);
    file_menu.setTitle(ns_string!("File"));
    file_menu.addItem(&new_window_item(mtm, target));
    file.setSubmenu(Some(&file_menu));
    menubar.insertItem_atIndex(&file, 1);

    Ok(())
}

/// Set the Dock icon. Winit finishes launching after the app-creation
/// callback and can replace an icon set earlier, so call this from a frame.
/// Also covers cargo-run and symlinked launches, where AppKit finds no bundle
/// ICNS.
pub fn apply_icon() -> Result<()> {
    let mtm = MainThreadMarker::new().context("macOS icons require the main thread")?;
    let data = NSData::with_bytes(include_bytes!("../assets/slerm.png"));
    let icon = NSImage::initWithData(NSImage::alloc(), &data).context("Cannot decode app icon")?;
    unsafe {
        NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&icon));
    }
    Ok(())
}
