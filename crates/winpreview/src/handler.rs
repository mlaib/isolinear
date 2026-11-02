//! The COM preview handler and its class factory. Explorer's preview host calls, on one thread:
//! `Initialize` (the file), `SetWindow` (the pane), `DoPreview`, `SetRect` when the pane is
//! resized, and `Unload`. The page is written to a private temporary file and shown in WebView2.

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use webview2_com::Microsoft::Web::WebView2::Win32::{
    CreateCoreWebView2EnvironmentWithOptions, ICoreWebView2Controller, ICoreWebView2Environment,
    ICoreWebView2EnvironmentOptions,
};
use webview2_com::{
    CoreWebView2EnvironmentOptions, CreateCoreWebView2ControllerCompletedHandler,
    CreateCoreWebView2EnvironmentCompletedHandler, NavigationStartingEventHandler,
    NewWindowRequestedEventHandler, take_pwstr, wait_with_pump,
};
use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_FAIL, E_INVALIDARG, E_NOTIMPL, E_POINTER,
    E_UNEXPECTED, HWND, RECT, S_FALSE, S_OK,
};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::Win32::System::Ole::{
    IObjectWithSite, IObjectWithSite_Impl, IOleWindow, IOleWindow_Impl,
};
use windows::Win32::UI::Input::KeyboardAndMouse::GetFocus;
use windows::Win32::UI::Shell::PropertiesSystem::{IInitializeWithFile, IInitializeWithFile_Impl};
use windows::Win32::UI::Shell::{IPreviewHandler, IPreviewHandler_Impl};
use windows::Win32::UI::WindowsAndMessaging::MSG;
use windows::core::{
    BOOL, GUID, HRESULT, HSTRING, IUnknown, Interface, PCWSTR, PWSTR, Ref, implement,
};

/// The class registered under [`crate::CLSID`].
pub const CLASS: GUID = GUID::from_u128(0x39085b8b_a5f4_4a5b_9c0e_94459b4e55b2);

static OBJECTS: AtomicUsize = AtomicUsize::new(0);
static LOCKS: AtomicUsize = AtomicUsize::new(0);
static PAGES: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// One WebView2 environment per preview host thread: creating it is the slow part.
    static ENVIRONMENT: RefCell<Option<ICoreWebView2Environment>> = const { RefCell::new(None) };
}

#[derive(Default)]
struct State {
    path: Option<PathBuf>,
    parent: HWND,
    rect: RECT,
    controller: Option<ICoreWebView2Controller>,
    page: Option<PathBuf>,
    site: Option<IUnknown>,
}

#[implement(IPreviewHandler, IInitializeWithFile, IObjectWithSite, IOleWindow)]
struct Handler {
    state: RefCell<State>,
}

impl Handler {
    fn new() -> Self {
        OBJECTS.fetch_add(1, Ordering::SeqCst);
        Self {
            state: RefCell::new(State::default()),
        }
    }
}

impl Drop for Handler {
    fn drop(&mut self) {
        close(&mut self.state.borrow_mut());
        OBJECTS.fetch_sub(1, Ordering::SeqCst);
    }
}

fn close(s: &mut State) {
    if let Some(c) = s.controller.take() {
        // SAFETY: a live controller of this thread.
        let _ = unsafe { c.Close() };
    }
    if let Some(page) = s.page.take() {
        let _ = std::fs::remove_file(page);
    }
}

/// The page for `path`, from the shared generator.
fn page(path: &Path) -> String {
    #[cfg(feature = "page")]
    {
        isolinear_preview::html(path)
    }
    #[cfg(not(feature = "page"))]
    {
        format!("<!doctype html><p>{}</p>", path.display())
    }
}

/// A `file:` URL for a local path, every byte outside the plain set percent-encoded.
fn file_url(path: &Path) -> String {
    let text = path.display().to_string().replace('\\', "/");
    let mut url = String::from("file:///");
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~:".contains(&b) {
            url.push(b as char);
        } else {
            url.push_str(&format!("%{b:02X}"));
        }
    }
    url
}

fn environment() -> windows::core::Result<ICoreWebView2Environment> {
    if let Some(env) = ENVIRONMENT.with(|e| e.borrow().clone()) {
        return Ok(env);
    }
    let base = std::env::var_os("LOCALAPPDATA").map_or_else(std::env::temp_dir, PathBuf::from);
    let data = HSTRING::from(base.join("Isolinear").join("PreviewWebView2").as_os_str());
    let (tx, rx) = std::sync::mpsc::channel();
    let options = CoreWebView2EnvironmentOptions::default();
    // SAFETY: the strings and the handler live across the call; the result comes back on this
    // thread while `wait_with_pump` runs its message loop.
    unsafe {
        CreateCoreWebView2EnvironmentWithOptions(
            PCWSTR::null(),
            &data,
            &ICoreWebView2EnvironmentOptions::from(options),
            &CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(move |error, env| {
                let result = error.and_then(|()| env.ok_or_else(|| E_POINTER.into()));
                tx.send(result).map_err(|_| E_UNEXPECTED.into())
            })),
        )?;
    }
    let env = wait_with_pump(rx).map_err(|_| windows::core::Error::from(E_FAIL))??;
    ENVIRONMENT.with(|e| *e.borrow_mut() = Some(env.clone()));
    Ok(env)
}

fn show(s: &mut State) -> windows::core::Result<()> {
    let path = s
        .path
        .clone()
        .ok_or(windows::core::Error::from(E_UNEXPECTED))?;
    close(s);
    let dir = std::env::temp_dir().join("IsolinearPreview");
    std::fs::create_dir_all(&dir).map_err(|_| windows::core::Error::from(E_FAIL))?;
    let file = dir.join(format!(
        "page-{}-{}.html",
        std::process::id(),
        PAGES.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::write(&file, page(&path)).map_err(|_| windows::core::Error::from(E_FAIL))?;
    s.page = Some(file.clone());
    let url = file_url(&file);

    let env = environment()?;
    let (tx, rx) = std::sync::mpsc::channel();
    // SAFETY: the parent window belongs to the preview host; the handler lives across the call.
    unsafe {
        env.CreateCoreWebView2Controller(
            s.parent,
            &CreateCoreWebView2ControllerCompletedHandler::create(Box::new(move |error, c| {
                let result = error.and_then(|()| c.ok_or_else(|| E_POINTER.into()));
                tx.send(result).map_err(|_| E_UNEXPECTED.into())
            })),
        )?;
    }
    let controller = wait_with_pump(rx).map_err(|_| windows::core::Error::from(E_FAIL))??;
    // SAFETY: COM calls on the controller just created, on its thread.
    unsafe {
        controller.SetBounds(s.rect)?;
        let view = controller.CoreWebView2()?;
        let settings = view.Settings()?;
        settings.SetIsScriptEnabled(false)?;
        settings.SetAreDefaultContextMenusEnabled(false)?;
        settings.SetAreDevToolsEnabled(false)?;
        settings.SetAreHostObjectsAllowed(false)?;
        settings.SetIsStatusBarEnabled(false)?;
        settings.SetIsWebMessageEnabled(false)?;
        // The page and nothing else: a link in the file does not load anything.
        let own = url.clone();
        let mut token = 0i64;
        view.add_NavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                if let Some(args) = args {
                    let mut uri = PWSTR::null();
                    args.Uri(&mut uri)?;
                    args.SetCancel(take_pwstr(uri) != own)?;
                }
                Ok(())
            })),
            &mut token,
        )?;
        view.add_NewWindowRequested(
            &NewWindowRequestedEventHandler::create(Box::new(|_, args| {
                if let Some(args) = args {
                    args.SetHandled(true)?;
                }
                Ok(())
            })),
            &mut token,
        )?;
        view.Navigate(&HSTRING::from(url))?;
        controller.SetIsVisible(true)?;
    }
    s.controller = Some(controller);
    Ok(())
}

impl IInitializeWithFile_Impl for Handler_Impl {
    fn Initialize(&self, pszfilepath: &PCWSTR, _grfmode: u32) -> windows::core::Result<()> {
        // SAFETY: the shell passes a valid NUL-terminated path.
        let path = unsafe { pszfilepath.to_string() }
            .map_err(|_| windows::core::Error::from(E_INVALIDARG))?;
        self.state.borrow_mut().path = Some(PathBuf::from(path));
        Ok(())
    }
}

impl IPreviewHandler_Impl for Handler_Impl {
    fn SetWindow(&self, hwnd: HWND, prc: *const RECT) -> windows::core::Result<()> {
        let mut s = self.state.borrow_mut();
        s.parent = hwnd;
        if !prc.is_null() {
            // SAFETY: the shell passes a valid rectangle.
            s.rect = unsafe { *prc };
        }
        if let Some(c) = &s.controller {
            // SAFETY: COM calls on this thread's controller.
            unsafe {
                c.SetParentWindow(hwnd)?;
                c.SetBounds(s.rect)?;
            }
        }
        Ok(())
    }

    fn SetRect(&self, prc: *const RECT) -> windows::core::Result<()> {
        if prc.is_null() {
            return Err(E_INVALIDARG.into());
        }
        let mut s = self.state.borrow_mut();
        // SAFETY: checked above.
        s.rect = unsafe { *prc };
        if let Some(c) = &s.controller {
            // SAFETY: COM call on this thread's controller.
            unsafe { c.SetBounds(s.rect)? };
        }
        Ok(())
    }

    fn DoPreview(&self) -> windows::core::Result<()> {
        let mut s = self.state.borrow_mut();
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| show(&mut s)))
            .unwrap_or_else(|_| Err(E_FAIL.into()))
    }

    fn Unload(&self) -> windows::core::Result<()> {
        let mut s = self.state.borrow_mut();
        close(&mut s);
        s.path = None;
        Ok(())
    }

    fn SetFocus(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn QueryFocus(&self) -> windows::core::Result<HWND> {
        // SAFETY: plain Win32 call.
        Ok(unsafe { GetFocus() })
    }

    fn TranslateAccelerator(&self, _pmsg: *const MSG) -> windows::core::Result<()> {
        // Not handled: the host processes the key.
        Err(windows::core::Error::from_hresult(S_FALSE))
    }
}

impl IObjectWithSite_Impl for Handler_Impl {
    fn SetSite(&self, punksite: Ref<IUnknown>) -> windows::core::Result<()> {
        self.state.borrow_mut().site = punksite.cloned();
        Ok(())
    }

    fn GetSite(&self, riid: *const GUID, ppvsite: *mut *mut c_void) -> windows::core::Result<()> {
        match &self.state.borrow().site {
            // SAFETY: the caller passes valid out pointers.
            Some(site) => unsafe { site.query(riid, ppvsite).ok() },
            None => Err(E_FAIL.into()),
        }
    }
}

impl IOleWindow_Impl for Handler_Impl {
    fn GetWindow(&self) -> windows::core::Result<HWND> {
        Ok(self.state.borrow().parent)
    }

    fn ContextSensitiveHelp(&self, _fentermode: BOOL) -> windows::core::Result<()> {
        Err(E_NOTIMPL.into())
    }
}

#[implement(IClassFactory)]
struct Factory;

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(
        &self,
        punkouter: Ref<IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut c_void,
    ) -> windows::core::Result<()> {
        if !punkouter.is_null() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let handler: IPreviewHandler = Handler::new().into();
        // SAFETY: the caller passes valid out pointers.
        unsafe { handler.query(riid, ppvobject).ok() }
    }

    fn LockServer(&self, flock: BOOL) -> windows::core::Result<()> {
        if flock.as_bool() {
            LOCKS.fetch_add(1, Ordering::SeqCst);
        } else {
            LOCKS.fetch_sub(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

/// Called by COM to get the class factory.
///
/// # Safety
/// The pointers come from COM and are valid.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    if ppv.is_null() || rclsid.is_null() || riid.is_null() {
        return E_POINTER;
    }
    // SAFETY: checked above.
    unsafe {
        *ppv = std::ptr::null_mut();
        if *rclsid != CLASS {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        let factory: IClassFactory = Factory.into();
        factory.query(riid, ppv)
    }
}

/// Called by COM to know whether the DLL can be unloaded.
#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    if OBJECTS.load(Ordering::SeqCst) == 0 && LOCKS.load(Ordering::SeqCst) == 0 {
        S_OK
    } else {
        S_FALSE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls_are_encoded() {
        assert_eq!(
            file_url(Path::new(
                r"C:\Users\Ann Lee\AppData\Local\Temp\page é.html"
            )),
            "file:///C:/Users/Ann%20Lee/AppData/Local/Temp/page%20%C3%A9.html"
        );
    }

    #[test]
    fn the_class_factory_makes_a_preview_handler() {
        let mut ppv = std::ptr::null_mut();
        // SAFETY: valid pointers to GUIDs and an out pointer.
        let hr = unsafe { DllGetClassObject(&CLASS, &IClassFactory::IID, &mut ppv) };
        assert_eq!(hr, S_OK);
        // SAFETY: COM returned an IClassFactory.
        let factory = unsafe { IClassFactory::from_raw(ppv) };
        // SAFETY: plain COM call.
        let handler: IPreviewHandler = unsafe { factory.CreateInstance(None) }.unwrap();
        assert!(handler.cast::<IInitializeWithFile>().is_ok());
        assert!(handler.cast::<IObjectWithSite>().is_ok());
        assert!(handler.cast::<IOleWindow>().is_ok());
        drop(handler);
        drop(factory);
        assert_eq!(DllCanUnloadNow(), S_OK);
        let other = GUID::from_u128(1);
        // SAFETY: as above.
        assert_eq!(
            unsafe { DllGetClassObject(&other, &IClassFactory::IID, &mut ppv) },
            CLASS_E_CLASSNOTAVAILABLE
        );
    }
}
