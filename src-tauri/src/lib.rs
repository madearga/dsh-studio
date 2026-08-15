//! DSH Studio — a native window around a locally running `dsh web` backend.
//!
//! Own implementation: a `Supervisor` owns the backend child process, keeps it
//! alive with backoff, and tells the splash page where to navigate once the
//! loopback server accepts connections.

use std::io::{BufRead, BufReader};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tauri::{
    menu::{CheckMenuItem, MenuBuilder, MenuItem, PredefinedMenuItem, SubmenuBuilder},
    tray::TrayIconBuilder,
    AppHandle, Emitter, Manager, RunEvent, WindowEvent,
};
use tauri_plugin_opener::OpenerExt;

/// Message sent to the splash page whenever the backend state changes.
#[derive(Clone, serde::Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
enum BackendEvent {
    Booting,
    Ready { url: String },
    Crashed { note: Option<String> },
    Missing { note: Option<String> },
}

/// Everything shared between the supervisor thread, commands, and the UI.
struct Shared {
    child: Mutex<Option<Child>>,
    url: Mutex<Option<String>>,
    zoom: Mutex<f64>,
    exiting: AtomicBool,
    restart_token: AtomicBool,
    /// Selected dsh profile (None = default). Survives restarts.
    profile: Mutex<Option<String>>,
    /// Tail of the child's stdout+stderr, for crash diagnostics.
    log: Arc<Mutex<String>>,
    tray: Mutex<Option<tauri::tray::TrayIcon>>,
}

impl Shared {
    fn new() -> Self {
        Self {
            child: Mutex::new(None),
            url: Mutex::new(None),
            zoom: Mutex::new(1.0),
            exiting: AtomicBool::new(false),
            restart_token: AtomicBool::new(false),
            profile: Mutex::new(None),
            log: Arc::new(Mutex::new(String::new())),
            tray: Mutex::new(None),
        }
    }

    fn log_tail(&self) -> Option<String> {
        let s = self.log.lock().unwrap();
        if s.is_empty() {
            None
        } else {
            Some(s.chars().rev().take(900).collect::<String>().chars().rev().collect())
        }
    }
}

#[tauri::command]
fn backend_url(state: tauri::State<'_, Shared>) -> Option<String> {
    state.url.lock().unwrap().clone()
}

/// Kill the child; the supervisor notices and boots a fresh one.
#[tauri::command]
fn restart_backend(state: tauri::State<'_, Shared>) {
    state.restart_token.store(true, Ordering::SeqCst);
    if let Some(mut c) = state.child.lock().unwrap().take() {
        let _ = c.kill();
        let _ = c.wait();
    }
}

#[tauri::command]
fn quit(app: AppHandle) {
    app.exit(0);
}

/// dsh profiles installed under ~/.dsh/profiles.
fn list_profiles() -> Vec<String> {
    let Some(home) = std::env::var_os("HOME") else { return vec![] };
    let dir = std::path::Path::new(&home).join(".dsh").join("profiles");
    let Ok(entries) = std::fs::read_dir(dir) else { return vec![] };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    names
}

/// Where the `dsh` CLI comes from: explicit override, bundled copy, then PATH.
fn backend_command(app: &AppHandle) -> Option<Command> {
    if let Ok(bin) = std::env::var("DSH_BIN") {
        return Some(Command::new(bin));
    }

    // Bundled layout: <resources>/backend/node + backend/node_modules/@deepseek-ai/dsh
    if let Ok(dir) = app.path().resource_dir() {
        let root = dir.join("backend");
        let node = root.join(node_exe());
        let cli = root
            .join("node_modules")
            .join("@deepseek-ai")
            .join("dsh")
            .join("lib")
            .join("bin.js");
        if node.is_file() && cli.is_file() {
            let mut cmd = Command::new(&node);
            cmd.arg(&cli);
            return Some(cmd);
        }
    }

    Some(Command::new("dsh"))
}

#[cfg(target_os = "windows")]
fn node_exe() -> &'static str {
    "node.exe"
}
#[cfg(not(target_os = "windows"))]
fn node_exe() -> &'static str {
    "node"
}

/// Grab a free loopback port by asking the OS, then releasing it again.
fn free_port() -> std::io::Result<u16> {
    std::net::TcpListener::bind(("127.0.0.1", 0))?.local_addr().map(|a| a.port())
}

fn wait_for_server(port: u16, deadline: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    false
}

/// Keep the tail of a child stream in the shared log (capped, so it can't grow).
fn pump_log<R: std::io::Read + Send + 'static>(reader: R, shared: Arc<Mutex<String>>) {
    std::thread::spawn(move || {
        let mut buf = String::new();
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    buf.push_str(&line);
                    if buf.len() > 3000 {
                        let cut = buf.len() - 2000;
                        buf.drain(..cut);
                    }
                    *shared.lock().unwrap() = buf.clone();
                }
            }
        }
    });
}

fn emit(app: &AppHandle, event: BackendEvent) {
    let _ = app.emit("backend", &event);
}

/// Owns the backend for the lifetime of the app: boot → probe → serve,
/// restarting with growing backoff whenever the child dies unexpectedly.
fn supervise(app: AppHandle) {
    let state = app.state::<Shared>();
    let mut backoff_secs: u64 = 1;

    loop {
        if state.exiting.load(Ordering::SeqCst) {
            return;
        }

        let port = match free_port() {
            Ok(p) => p,
            Err(e) => {
                emit(&app, BackendEvent::Missing { note: Some(e.to_string()) });
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };

        let mut cmd = match backend_command(&app) {
            Some(c) => c,
            None => {
                emit(
                    &app,
                    BackendEvent::Missing { note: None },
                );
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };

        *state.log.lock().unwrap() = String::new();
        emit(&app, BackendEvent::Booting);

        let profile = state.profile.lock().unwrap().clone();
        if let Some(p) = &profile {
            cmd.env("DSH_PROFILE", p);
        }

        let spawned = cmd
            .args(["web", "--host", "127.0.0.1", "--port", &port.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();

        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                emit(&app, BackendEvent::Missing { note: Some(e.to_string()) });
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
        if let Some(out) = child.stdout.take() {
            pump_log(out, state.log.clone());
        }
        if let Some(err) = child.stderr.take() {
            pump_log(err, state.log.clone());
        }
        *state.child.lock().unwrap() = Some(child);

        if wait_for_server(port, Duration::from_secs(30)) {
            let url = format!("http://127.0.0.1:{port}");
            *state.url.lock().unwrap() = Some(url.clone());
            backoff_secs = 1;
            if let Some(tray) = state.tray.lock().unwrap().as_ref() {
                let _ = tray.set_tooltip(Some(format!("DSH Studio — {url}")));
            }
            emit(&app, BackendEvent::Ready { url });
        } else if let Some(mut c) = state.child.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }

        // Block until the child exits (or a manual restart kills it).
        loop {
            if state.exiting.load(Ordering::SeqCst) {
                return;
            }
            if state.restart_token.swap(false, Ordering::SeqCst) {
                break; // user asked for a fresh boot
            }
            let mut guard = state.child.lock().unwrap();
            match guard.as_mut().map(|c| c.try_wait()) {
                Some(Ok(Some(_))) | Some(Err(_)) | None => break,
                Some(Ok(None)) => {}
            }
            drop(guard);
            std::thread::sleep(Duration::from_millis(400));
        }
        if let Some(mut c) = state.child.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }

        if !state.exiting.load(Ordering::SeqCst) {
            let note = state.log_tail();
            emit(&app, BackendEvent::Crashed { note });
            std::thread::sleep(Duration::from_secs(backoff_secs));
            backoff_secs = (backoff_secs * 2).min(30);
        }
    }
}

fn menu(app: &AppHandle) -> tauri::Result<()> {
    let state = app.state::<Shared>();
    let selected = state.profile.lock().unwrap().clone();

    let app_menu = SubmenuBuilder::new(app, "DSH Studio")
        .item(&PredefinedMenuItem::about(app, None, None)?)
        .separator()
        .item(&PredefinedMenuItem::quit(app, Some("Quit DSH Studio"))?)
        .build()?;

    // ponytail: profiles are read at menu build; new profiles appear after an
    // app restart. Rebuild happens on selection, not on profile discovery.
    let mut backend = SubmenuBuilder::new(app, "Backend")
        .item(&MenuItem::with_id(
            app,
            "restart-backend",
            "Restart Backend",
            true,
            Some("CmdOrCtrl+Shift+R"),
        )?)
        .item(&MenuItem::with_id(
            app,
            "open-browser",
            "Open in Browser…",
            true,
            Some("CmdOrCtrl+Shift+B"),
        )?)
        .separator()
        .item(&CheckMenuItem::with_id(
            app,
            "profile:",
            "Default (no profile)",
            true,
            selected.is_none(),
            None::<&str>,
        )?);
    for p in list_profiles() {
        let checked = selected.as_deref() == Some(p.as_str());
        backend = backend.item(&CheckMenuItem::with_id(
            app,
            format!("profile:{p}"),
            p,
            true,
            checked,
            None::<&str>,
        )?);
    }
    let backend_menu = backend.build()?;

    let edit = SubmenuBuilder::new(app, "Edit")
        .items(&[
            &PredefinedMenuItem::undo(app, None)?,
            &PredefinedMenuItem::redo(app, None)?,
        ])
        .separator()
        .items(&[
            &PredefinedMenuItem::cut(app, None)?,
            &PredefinedMenuItem::copy(app, None)?,
            &PredefinedMenuItem::paste(app, None)?,
            &PredefinedMenuItem::select_all(app, None)?,
        ])
        .build()?;

    let view = SubmenuBuilder::new(app, "View")
        .item(&PredefinedMenuItem::fullscreen(app, None)?)
        .separator()
        .item(&MenuItem::with_id(app, "in", "Zoom In", true, Some("CmdOrCtrl+Plus"))?)
        .item(&MenuItem::with_id(app, "out", "Zoom Out", true, Some("CmdOrCtrl+-"))?)
        .item(&MenuItem::with_id(app, "fit", "Actual Size", true, Some("CmdOrCtrl+0"))?)
        .separator()
        .item(&MenuItem::with_id(app, "reload", "Reload", true, Some("CmdOrCtrl+R"))?)
        .item(&MenuItem::with_id(
            app,
            "devtools",
            "Developer Tools",
            true,
            Some("CmdOrCtrl+Shift+I"),
        )?)
        .build()?;

    let window = SubmenuBuilder::new(app, "Window")
        .item(&PredefinedMenuItem::minimize(app, None)?)
        .item(&PredefinedMenuItem::maximize(app, None)?)
        .separator()
        .item(&PredefinedMenuItem::close_window(app, None)?)
        .build()?;

    app.set_menu(
        MenuBuilder::new(app)
            .items(&[&app_menu, &backend_menu, &edit, &view, &window])
            .build()?,
    )?;
    Ok(())
}

fn tray(app: &AppHandle) -> tauri::Result<tauri::tray::TrayIcon> {
    let show = MenuItem::with_id(app, "show", "Show DSH Studio", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let m = MenuBuilder::new(app).items(&[&show, &quit]).build()?;

    let icon = app.default_window_icon().unwrap().clone();
    TrayIconBuilder::new()
        .icon(icon)
        .tooltip("DSH Studio")
        .menu(&m)
        .on_menu_event(|app, e| match e.id().as_ref() {
            "show" => reveal(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, e| {
            if matches!(e, tauri::tray::TrayIconEvent::Click { .. }) {
                reveal(tray.app_handle());
            }
        })
        .build(app)
}

fn reveal(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

fn open_in_browser(app: &AppHandle) {
    let Some(url) = app.state::<Shared>().url.lock().unwrap().clone() else {
        return;
    };
    let _ = app.opener().open_url(url, None::<&str>);
}

fn set_zoom(app: &AppHandle, factor: f64) {
    let Some(w) = app.get_webview_window("main") else { return };
    let state = app.state::<Shared>();
    let mut z = state.zoom.lock().unwrap();
    *z = if factor == 0.0 { 1.0 } else { (*z * factor).clamp(0.5, 2.0) };
    let _ = w.set_zoom(*z);
}

fn reload(app: &AppHandle) {
    let Some(w) = app.get_webview_window("main") else { return };
    match app.state::<Shared>().url.lock().unwrap().clone() {
        Some(u) => {
            if let Ok(url) = tauri::Url::parse(&u) {
                let _ = w.navigate(url);
            }
        }
        None => {
            let _ = w.eval("location.reload()");
        }
    }
}

fn toggle_devtools(app: &AppHandle) {
    let Some(w) = app.get_webview_window("main") else { return };
    if w.is_devtools_open() {
        let _ = w.close_devtools();
    } else {
        let _ = w.open_devtools();
    }
}

fn is_local(url: &tauri::Url) -> bool {
    match url.host_str() {
        Some("127.0.0.1") | Some("localhost") | Some("tauri.localhost") => true,
        _ => url.scheme().starts_with("tauri"),
    }
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_window_state::Builder::default().build())
        .plugin(tauri_plugin_opener::init())
        .manage(Shared::new())
        .invoke_handler(tauri::generate_handler![
            backend_url,
            restart_backend,
            quit
        ])
        .setup(|app| {
            let h = app.handle();
            menu(h)?;
            let t = tray(h)?;
            *h.state::<Shared>().tray.lock().unwrap() = Some(t);

            // Build the main window here so we can guard navigation:
            // the dsh UI stays inside; external links open in the browser.
            let window = tauri::WebviewWindowBuilder::new(
                h,
                "main",
                tauri::WebviewUrl::default(),
            )
            .title("DSH Studio")
            .inner_size(1280.0, 820.0)
            .min_inner_size(760.0, 520.0)
            .center()
            .on_navigation(|url| {
                if is_local(url) {
                    return true;
                }
                let _ = tauri_plugin_opener::open_url(url.to_string(), None::<&str>);
                false
            })
            .build()?;

            let wh = window.app_handle().clone();
            window.on_window_event(move |event| {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    if let Some(w) = wh.get_webview_window("main") {
                        let _ = w.hide();
                    }
                }
            });
            drop(window);

            let handle = h.clone();
            std::thread::spawn(move || supervise(handle));
            Ok(())
        })
        .on_menu_event(|app, e| match e.id().as_ref() {
            "in" => set_zoom(app, 1.1),
            "out" => set_zoom(app, 0.9),
            "fit" => set_zoom(app, 0.0),
            "reload" => reload(app),
            "devtools" => toggle_devtools(app),
            "restart-backend" => restart_backend(app.state::<Shared>()),
            "open-browser" => open_in_browser(app),
            id if id.starts_with("profile:") => {
                let p = &id["profile:".len()..];
                *app.state::<Shared>().profile.lock().unwrap() =
                    if p.is_empty() { None } else { Some(p.to_string()) };
                let _ = menu(app); // refresh checkmarks
                restart_backend(app.state::<Shared>());
            }
            _ => {}
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                // Closing hides to the tray so the backend keeps running.
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("failed to assemble DSH Studio")
        .run(|app, event| {
            if let RunEvent::ExitRequested { .. } = event {
                let state = app.state::<Shared>();
                state.exiting.store(true, Ordering::SeqCst);
                let mut guard = state.child.lock().unwrap();
                if let Some(c) = guard.as_mut() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
                drop(guard);
            }
        });
}
