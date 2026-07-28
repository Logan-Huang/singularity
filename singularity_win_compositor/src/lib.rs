//! The Windows counterpart of `singularity_wl_compositor`.
//!
//! The Wayland version runs a nested Wayland server that client apps connect to.
//! Windows has no nested-display-server concept, so this instead:
//! - spawns the program as a normal process,
//! - finds the top-level window it creates,
//! - captures that window into the shared image every frame (`PrintWindow`),
//! - forwards key events to it with posted window messages.
#![cfg(windows)]

use image::RgbaImage;
use singularity_sttk::nodular_applet::{
    AppletSpawner, AppletSpawnerTrait, NodularApplet, NodularAppletInitializer, NodularRunnerHook,
    recursive_node_applet::RecursiveNodeApplet,
};
use sonamu_ui::ui_event::{Key, UIEvent};
use std::{
    collections::BTreeSet,
    path::Path,
    sync::{Arc, Mutex, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{CloseHandle, HWND, LPARAM, WPARAM},
        System::Threading::{
            OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW,
        },
        UI::{
            Input::KeyboardAndMouse::{
                VK_BACK, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT,
                VK_UP,
            },
            WindowsAndMessaging::{
                EnumWindows, GUITHREADINFO, GetGUIThreadInfo, GetWindowThreadProcessId, IsWindow,
                IsWindowVisible, PostMessageW, WM_CHAR, WM_KEYDOWN, WM_KEYUP,
            },
        },
    },
    core::{BOOL, PWSTR},
};

mod applet_impls;
mod capture;

/// Currently responsible for running the embedded app,
/// and saving its window's pixels to a shared image
struct WindowsCompositor {
    /// The embedded app's top-level window.
    /// Never leaves the compositor thread (HWND is not Send).
    target_window: HWND,

    image: Arc<Mutex<Option<RgbaImage>>>,
    input_queue: mpsc::Receiver<UIEvent>,
}
impl WindowsCompositor {
    /// Matches the Wayland compositor's frame cadence
    const FRAME_MILLIS: u64 = 100;
    /// Slow apps (or store-app aliases that relaunch themselves) can take a while
    /// to show a window
    const FIND_WINDOW_TIMEOUT: Duration = Duration::from_secs(15);

    /// Creates and runs
    /// NOTE: this should be run in a thread that isn't the main thread
    fn new(
        image: Arc<Mutex<Option<RgbaImage>>>,
        program: String,
        input_queue: mpsc::Receiver<UIEvent>,
    ) {
        let exe_stem = exe_stem(&program).unwrap_or_else(|| program.to_lowercase());

        // Windows that exist before we spawn can never be the app we launched,
        // so the name-based fallback must not grab them.
        let preexisting_windows = all_visible_windows();

        let child = std::process::Command::new(&program).spawn().ok();
        let child_pid = child.as_ref().map_or(0, std::process::Child::id);

        let deadline = Instant::now() + Self::FIND_WINDOW_TIMEOUT;
        let target_window = loop {
            if let Some(window) = find_target_window(child_pid, &exe_stem, &preexisting_windows) {
                break window;
            }
            if Instant::now() >= deadline {
                log::warn!("Never found a window for embedded app `{program}`; giving up.");
                return;
            }
            thread::sleep(Duration::from_millis(200));
        };

        let compositor = Self {
            target_window,
            image,
            input_queue,
        };
        compositor.run();
    }

    fn run(mut self) {
        loop {
            while let Ok(ui_event) = self.input_queue.try_recv() {
                self.process_ui_event(ui_event);
            }

            if !unsafe { IsWindow(Some(self.target_window)) }.as_bool() {
                log::info!("Embedded app's window is gone; stopping capture.");
                break;
            }

            if let Some(frame) = capture::capture_window(self.target_window) {
                *self.image.lock().unwrap() = Some(frame);
            }

            thread::sleep(Duration::from_millis(Self::FRAME_MILLIS));
        }
    }

    fn process_ui_event(&mut self, ui_event: UIEvent) {
        match ui_event {
            UIEvent::KeyPress(key, _key_modifiers) => self.send_key(key),
            UIEvent::WindowResized(_) => {}
            UIEvent::MousePress(_, _display_area) => {
                log::debug!("TODO: forward mouse presses to the embedded app");
            }
        }
    }

    /// NOTE: ignores modifiers (like the Wayland version)
    fn send_key(&self, key: Key) {
        let target = self.focus_window();

        match key {
            // WM_CHAR carries the already-translated character,
            // so case and symbols come through without faking shift state
            Key::Char(c) => post_message(target, WM_CHAR, WPARAM(c as usize), LPARAM(1)),
            other => {
                let virtual_key = match other {
                    Key::ArrowKeyUp => VK_UP,
                    Key::ArrowKeyDown => VK_DOWN,
                    Key::ArrowKeyLeft => VK_LEFT,
                    Key::ArrowKeyRight => VK_RIGHT,
                    Key::Enter => VK_RETURN,
                    Key::Backspace => VK_BACK,
                    Key::PageUp => VK_PRIOR,
                    Key::PageDown => VK_NEXT,
                    Key::Escape => VK_ESCAPE,
                    Key::Char(_) => unreachable!(),
                };
                post_message(target, WM_KEYDOWN, WPARAM(virtual_key.0 as usize), LPARAM(1));
                // lparam bits 30/31: key was down, and this is a release
                post_message(
                    target,
                    WM_KEYUP,
                    WPARAM(virtual_key.0 as usize),
                    LPARAM(0xC000_0001_u32 as i32 as isize),
                );
            }
        }
    }

    /// Posting to the top-level window misses child controls (e.g. an edit box),
    /// so ask the app's UI thread which of its windows has focus.
    fn focus_window(&self) -> HWND {
        let thread_id = unsafe { GetWindowThreadProcessId(self.target_window, None) };

        let mut info = GUITHREADINFO {
            cbSize: size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        if unsafe { GetGUIThreadInfo(thread_id, &mut info) }.is_ok() && !info.hwndFocus.is_invalid()
        {
            info.hwndFocus
        } else {
            self.target_window
        }
    }
}

fn post_message(target: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) {
    if let Err(err) = unsafe { PostMessageW(Some(target), message, wparam, lparam) } {
        log::warn!("Failed to forward input to embedded app: {err}");
    }
}

fn exe_stem(program: &str) -> Option<String> {
    Some(
        Path::new(program)
            .file_stem()?
            .to_string_lossy()
            .to_lowercase(),
    )
}

/// The executable name (without path or extension) behind a pid
fn process_exe_stem(pid: u32) -> Option<String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;

        let mut buffer = [0u16; 1024];
        let mut length = buffer.len() as u32;
        let result = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        );
        let _ = CloseHandle(process);
        result.ok()?;

        exe_stem(&String::from_utf16_lossy(&buffer[..length as usize]))
    }
}

/// HWNDs are stored as isize because the search state crosses the
/// `EnumWindows` callback boundary as a raw pointer
struct FindTargetWindow {
    pid: u32,
    exe_stem: String,
    exclude: BTreeSet<isize>,
    pid_match: Option<isize>,
    name_match: Option<isize>,
}

/// Finds the embedded app's top-level window.
///
/// A pid match is definitive. The name match is a fallback for programs like
/// Windows 11's notepad, where the spawned exe is an alias that hands off to a
/// store app in a different process.
fn find_target_window(pid: u32, exe_stem: &str, exclude: &BTreeSet<isize>) -> Option<HWND> {
    let mut search = FindTargetWindow {
        pid,
        exe_stem: exe_stem.to_string(),
        exclude: exclude.clone(),
        pid_match: None,
        name_match: None,
    };

    unsafe {
        // EnumWindows reports an error whenever the callback stops the search
        // early, so its result is meaningless here
        let _ = EnumWindows(
            Some(find_target_window_callback),
            LPARAM(&raw mut search as isize),
        );
    }

    search
        .pid_match
        .or(search.name_match)
        .map(|raw| HWND(raw as *mut core::ffi::c_void))
}

unsafe extern "system" fn find_target_window_callback(window: HWND, lparam: LPARAM) -> BOOL {
    unsafe {
        let search = &mut *(lparam.0 as *mut FindTargetWindow);

        if !IsWindowVisible(window).as_bool() || search.exclude.contains(&(window.0 as isize)) {
            return true.into();
        }

        let mut window_pid = 0u32;
        GetWindowThreadProcessId(window, Some(&raw mut window_pid));

        if window_pid == search.pid {
            search.pid_match = Some(window.0 as isize);
            // stop enumerating
            return false.into();
        }
        if search.name_match.is_none()
            && process_exe_stem(window_pid).is_some_and(|stem| stem == search.exe_stem)
        {
            search.name_match = Some(window.0 as isize);
        }

        true.into()
    }
}

fn all_visible_windows() -> BTreeSet<isize> {
    let mut windows = BTreeSet::new();

    unsafe extern "system" fn collect_callback(window: HWND, lparam: LPARAM) -> BOOL {
        unsafe {
            let windows = &mut *(lparam.0 as *mut BTreeSet<isize>);
            if IsWindowVisible(window).as_bool() {
                windows.insert(window.0 as isize);
            }
        }
        true.into()
    }

    unsafe {
        let _ = EnumWindows(Some(collect_callback), LPARAM(&raw mut windows as isize));
    }

    windows
}

pub struct WindowsApplet {
    image: Arc<Mutex<Option<RgbaImage>>>,
    _thread: JoinHandle<()>,
    input_sender: mpsc::Sender<UIEvent>,
    hook: Box<dyn NodularRunnerHook>,
}
impl WindowsApplet {
    #[must_use]
    pub fn new(hook: Box<dyn NodularRunnerHook>, program: String) -> Self {
        let image = Arc::new(Mutex::new(None));

        let (tx, rx) = mpsc::channel();

        let image_clone = image.clone();
        let thread = thread::spawn(|| WindowsCompositor::new(image_clone, program, rx));

        Self {
            image,
            _thread: thread,
            input_sender: tx,
            hook,
        }
    }

    pub fn get_initiator(program: String) -> impl FnOnce(Box<dyn NodularRunnerHook>) -> Self {
        |hook: Box<dyn NodularRunnerHook>| Self::new(hook, program)
    }
    pub fn get_boxed_initiator(
        program: String,
    ) -> impl FnOnce(Box<dyn NodularRunnerHook>) -> Box<dyn NodularApplet> {
        |hook: Box<dyn NodularRunnerHook>| Box::new(Self::new(hook, program))
    }
    #[must_use]
    pub fn get_applet_spawner() -> AppletSpawner {
        struct WindowsSpawner;
        impl AppletSpawnerTrait for WindowsSpawner {
            fn create_initializer(&self, args: &[&str]) -> Option<NodularAppletInitializer> {
                let program = args.first().unwrap_or(&"notepad");

                Some(RecursiveNodeApplet::boxed_get_boxed_initializer(
                    WindowsApplet::get_boxed_initiator(program.to_string()),
                ))
            }

            fn duplicate(&self) -> AppletSpawner {
                Box::new(Self)
            }
        }
        Box::new(WindowsSpawner)
    }
}
