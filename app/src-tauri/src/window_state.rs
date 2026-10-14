//! Main-window placement belongs to this device, never to a shared library.

#[cfg(windows)]
mod windows {
    use ::windows::Win32::Foundation::RECT;
    use ::windows::Win32::UI::WindowsAndMessaging::{
        GetWindowPlacement, GetWindowRect, SetWindowPos, SWP_NOACTIVATE, SWP_NOOWNERZORDER,
        SWP_NOSIZE, SWP_NOZORDER, SW_SHOWMAXIMIZED, SW_SHOWMINIMIZED, WINDOWPLACEMENT,
    };
    use anyhow::{ensure, Context, Result};
    use serde::{Deserialize, Serialize};
    use std::{
        io::Write,
        path::{Path, PathBuf},
        sync::{Arc, Mutex},
        time::Duration,
    };
    use tauri::{AppHandle, Manager, Window, WindowEvent};

    const FILE_NAME: &str = "window-state.json";
    const MAX_STATE_BYTES: u64 = 4096;

    #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Bounds {
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    }

    impl Bounds {
        fn valid(&self) -> bool {
            self.x.abs_diff(0) <= 1_000_000
                && self.y.abs_diff(0) <= 1_000_000
                && (1..=100_000).contains(&self.width)
                && (1..=100_000).contains(&self.height)
        }
    }

    #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Display {
        name: Option<String>,
        work_area: Bounds,
        scale_factor: f64,
    }

    impl Display {
        fn from_monitor(monitor: &tauri::Monitor) -> Self {
            let work = monitor.work_area();
            Self {
                name: monitor.name().cloned(),
                work_area: Bounds {
                    x: work.position.x,
                    y: work.position.y,
                    width: work.size.width,
                    height: work.size.height,
                },
                scale_factor: monitor.scale_factor(),
            }
        }

        fn valid(&self) -> bool {
            self.work_area.valid()
                && self.scale_factor.is_finite()
                && (0.5..=8.0).contains(&self.scale_factor)
                && self.name.as_ref().is_none_or(|name| name.len() <= 200)
        }
    }

    #[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Placement {
        version: u32,
        normal_bounds: Bounds,
        display: Display,
        maximized: bool,
    }

    impl Placement {
        fn valid(&self) -> bool {
            self.version == 1 && self.normal_bounds.valid() && self.display.valid()
        }
    }

    struct Saved {
        placement: Option<Placement>,
        written: Option<Placement>,
        restoring: bool,
        pending: bool,
    }

    #[derive(Clone)]
    struct WindowState {
        path: PathBuf,
        saved: Arc<Mutex<Saved>>,
    }

    fn read(path: &Path) -> Option<Placement> {
        if std::fs::metadata(path).ok()?.len() > MAX_STATE_BYTES {
            return None;
        }
        let placement: Placement = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
        placement.valid().then_some(placement)
    }

    // Translate native workspace coordinates before using a screen-coordinate API.
    // In particular, a top/left taskbar must not make the window creep each launch.
    fn screen_bounds(normal: Bounds, monitor_x: i32, monitor_y: i32, work: &Bounds) -> Bounds {
        Bounds {
            x: normal.x.saturating_add(work.x.saturating_sub(monitor_x)),
            y: normal.y.saturating_add(work.y.saturating_sub(monitor_y)),
            ..normal
        }
    }

    fn restored(saved: &Placement, displays: &[Display], primary: usize) -> Option<Placement> {
        let same_display = displays
            .iter()
            .position(|display| saved.display.name.is_some() && display.name == saved.display.name);
        let target = displays.get(same_display.unwrap_or(primary))?;
        if !saved.valid() || !target.valid() {
            return None;
        }
        let work = &target.work_area;
        let ratio = target.scale_factor / saved.display.scale_factor;
        let scaled = |value: f64| (value * ratio).round() as i64;
        let width = scaled(f64::from(saved.normal_bounds.width))
            .max((640.0 * target.scale_factor).round() as i64)
            .min(i64::from(work.width)) as u32;
        let height = scaled(f64::from(saved.normal_bounds.height))
            .max((600.0 * target.scale_factor).round() as i64)
            .min(i64::from(work.height)) as u32;
        let (x, y) = if same_display.is_some() {
            (
                i64::from(work.x)
                    + scaled(
                        f64::from(saved.normal_bounds.x) - f64::from(saved.display.work_area.x),
                    ),
                i64::from(work.y)
                    + scaled(
                        f64::from(saved.normal_bounds.y) - f64::from(saved.display.work_area.y),
                    ),
            )
        } else {
            (
                i64::from(work.x) + i64::from(work.width - width) / 2,
                i64::from(work.y) + i64::from(work.height - height) / 2,
            )
        };
        Some(Placement {
            version: 1,
            normal_bounds: Bounds {
                x: x.clamp(
                    i64::from(work.x),
                    i64::from(work.x) + i64::from(work.width - width),
                ) as i32,
                y: y.clamp(
                    i64::from(work.y),
                    i64::from(work.y) + i64::from(work.height - height),
                ) as i32,
                width,
                height,
            },
            display: target.clone(),
            maximized: saved.maximized,
        })
    }

    fn capture(window: &Window, previous_maximized: bool) -> Result<Placement> {
        let mut native = WINDOWPLACEMENT {
            length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
            ..Default::default()
        };
        let hwnd = window.hwnd()?;
        unsafe { GetWindowPlacement(hwnd, &mut native)? };
        let monitor = window
            .current_monitor()?
            .context("window_monitor_unavailable")?;
        let display = Display::from_monitor(&monitor);
        let rect = native.rcNormalPosition;
        let mut normal = Bounds {
            x: rect.left,
            y: rect.top,
            width: u32::try_from(i64::from(rect.right) - i64::from(rect.left))?,
            height: u32::try_from(i64::from(rect.bottom) - i64::from(rect.top))?,
        };
        normal = screen_bounds(
            normal,
            monitor.position().x,
            monitor.position().y,
            &display.work_area,
        );
        if native.showCmd != SW_SHOWMINIMIZED.0 as u32
            && native.showCmd != SW_SHOWMAXIMIZED.0 as u32
        {
            // Snap layouts may retain a separate pre-snap restore rectangle.
            // A normal window remembers its actual last screen bounds instead.
            let mut rect = RECT::default();
            unsafe { GetWindowRect(hwnd, &mut rect)? };
            normal = Bounds {
                x: rect.left,
                y: rect.top,
                width: u32::try_from(i64::from(rect.right) - i64::from(rect.left))?,
                height: u32::try_from(i64::from(rect.bottom) - i64::from(rect.top))?,
            };
        }
        let placement = Placement {
            version: 1,
            normal_bounds: normal,
            display,
            // GetWindowPlacement preserves the normal rect while minimized, but
            // does not report whether it was maximized before minimizing.
            maximized: if native.showCmd == SW_SHOWMINIMIZED.0 as u32 || !window.is_visible()? {
                previous_maximized
            } else {
                native.showCmd == SW_SHOWMAXIMIZED.0 as u32
            },
        };
        ensure!(placement.valid(), "window_placement_invalid");
        Ok(placement)
    }

    // Every writer holds this mutex through atomic replacement, and reads the
    // current placement rather than an earlier queued snapshot.
    fn write(state: &WindowState, saved: &mut Saved) -> Result<()> {
        let Some(placement) = saved.placement.as_ref() else {
            return Ok(());
        };
        if saved.written.as_ref() == Some(placement) {
            return Ok(());
        }
        let temporary = state.path.with_extension("json.pending");
        let result = (|| {
            let mut file = std::fs::File::create(&temporary)?;
            file.write_all(&serde_json::to_vec(placement)?)?;
            file.sync_all()?;
            drop(file);
            crate::ai_provider::replace_file(&temporary, &state.path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(temporary);
        } else {
            saved.written = Some(placement.clone());
        }
        result
    }

    fn remember(window: &Window, immediate: bool) {
        let Some(state) = window.app_handle().try_state::<WindowState>() else {
            return;
        };
        let previous_maximized = {
            let Ok(saved) = state.saved.lock() else {
                return;
            };
            if saved.restoring {
                return;
            }
            saved
                .placement
                .as_ref()
                .is_some_and(|placement| placement.maximized)
        };
        let placement = capture(window, previous_maximized).ok();
        if placement.is_none() && !immediate {
            return;
        }
        let Ok(mut saved) = state.saved.lock() else {
            return;
        };
        if placement.is_some() {
            saved.placement = placement;
        }
        if immediate {
            let _ = write(&state, &mut saved);
        } else if !saved.pending {
            saved.pending = true;
            let state = state.inner().clone();
            tauri::async_runtime::spawn(async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                let Ok(mut saved) = state.saved.lock() else {
                    return;
                };
                saved.pending = false;
                let _ = write(&state, &mut saved);
            });
        }
    }

    pub(super) fn initialize(app: &mut tauri::App, device_dir: &Path, visible: bool) {
        let path = device_dir.join(FILE_NAME);
        let placement = read(&path);
        app.manage(WindowState {
            path,
            saved: Arc::new(Mutex::new(Saved {
                placement: placement.clone(),
                written: placement.clone(),
                restoring: true,
                pending: false,
            })),
        });
        let Some(window) = app.get_window("main") else {
            return;
        };
        let restored = placement.as_ref().and_then(|placement| {
            let monitors = window.available_monitors().ok()?;
            let displays: Vec<_> = monitors.iter().map(Display::from_monitor).collect();
            let primary = window
                .primary_monitor()
                .ok()
                .flatten()
                .and_then(|primary| {
                    displays
                        .iter()
                        .position(|display| *display == Display::from_monitor(&primary))
                })
                .unwrap_or(0);
            restored(placement, &displays, primary)
        });
        if let Some(placement) = &restored {
            let bounds = &placement.normal_bounds;
            if let Ok(hwnd) = window.hwnd() {
                // Finish a cross-monitor DPI transition before setting the final
                // physical size, so WM_DPICHANGED cannot scale it a second time.
                let _ = unsafe {
                    SetWindowPos(
                        hwnd,
                        None,
                        bounds.x,
                        bounds.y,
                        0,
                        0,
                        SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER | SWP_NOSIZE,
                    )
                };
                let _ = unsafe {
                    SetWindowPos(
                        hwnd,
                        None,
                        bounds.x,
                        bounds.y,
                        bounds.width as i32,
                        bounds.height as i32,
                        SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER,
                    )
                };
            }
        }
        if visible {
            let _ = window.show();
            if restored
                .as_ref()
                .is_some_and(|placement| placement.maximized)
            {
                let _ = window.maximize();
            }
        }
        if let Ok(mut saved) = app.state::<WindowState>().saved.lock() {
            if restored.is_some() {
                saved.placement = restored;
            }
            saved.restoring = false;
        }
        remember(&window, false);
    }

    pub(super) fn on_event(window: &Window, event: &WindowEvent) {
        if window.label() != "main" {
            return;
        }
        match event {
            WindowEvent::Moved(_)
            | WindowEvent::Resized(_)
            | WindowEvent::ScaleFactorChanged { .. } => remember(window, false),
            WindowEvent::Focused(false) | WindowEvent::CloseRequested { .. } => {
                remember(window, true)
            }
            _ => {}
        }
    }

    pub(super) async fn flush_app(app: &AppHandle) {
        let handle = app.clone();
        let (finished, receiver) = tokio::sync::oneshot::channel();
        if app
            .run_on_main_thread(move || {
                if let Some(window) = handle.get_window("main") {
                    remember(&window, true);
                }
                let _ = finished.send(());
            })
            .is_ok()
        {
            let _ = receiver.await;
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn display(name: &str, x: i32, y: i32, width: u32, height: u32, scale: f64) -> Display {
            Display {
                name: Some(name.into()),
                work_area: Bounds {
                    x,
                    y,
                    width,
                    height,
                },
                scale_factor: scale,
            }
        }

        fn placement() -> Placement {
            Placement {
                version: 1,
                normal_bounds: Bounds {
                    x: -1720,
                    y: 120,
                    width: 1280,
                    height: 800,
                },
                display: display("LEFT", -1920, 0, 1920, 1040, 1.0),
                maximized: false,
            }
        }

        #[test]
        fn same_display_keeps_negative_position_and_exact_normal_size() {
            let saved = placement();
            assert_eq!(restored(&saved, &[saved.display.clone()], 0), Some(saved));
        }

        #[test]
        fn dpi_and_display_origin_changes_keep_logical_size_and_offset() {
            let saved = placement();
            let target = display("LEFT", -3840, -1600, 3840, 2080, 2.0);
            let next = restored(&saved, &[target.clone()], 0).unwrap();
            assert_eq!(
                next.normal_bounds,
                Bounds {
                    x: -3440,
                    y: -1360,
                    width: 2560,
                    height: 1600
                }
            );
            assert_eq!(next.display, target);
        }

        #[test]
        fn removed_monitor_falls_back_to_primary_and_fits_work_area() {
            let mut saved = placement();
            saved.maximized = true;
            let primary = display("PRIMARY", 80, 40, 1024, 700, 1.0);
            let next = restored(
                &saved,
                &[display("OTHER", -2560, 0, 2560, 1400, 1.0), primary.clone()],
                1,
            )
            .unwrap();
            assert_eq!(next.normal_bounds, primary.work_area);
            assert!(next.maximized);
        }

        #[test]
        fn changed_work_area_clamps_position_without_taskbar_creep() {
            let mut saved = placement();
            saved.normal_bounds = Bounds {
                x: 10,
                y: 5,
                width: 1280,
                height: 800,
            };
            saved.display = display("MAIN", 0, 0, 1920, 1080, 1.0);
            let target = display("MAIN", 64, 48, 1856, 1032, 1.0);
            let next = restored(&saved, &[target.clone()], 0).unwrap();
            assert_eq!(next.normal_bounds.x, 74);
            assert_eq!(next.normal_bounds.y, 53);
            assert_eq!(restored(&next, &[target], 0), Some(next));
            assert_eq!(
                screen_bounds(
                    Bounds {
                        x: -1900,
                        y: -900,
                        width: 1280,
                        height: 800
                    },
                    -1920,
                    -1080,
                    &Bounds {
                        x: -1872,
                        y: -1040,
                        width: 1872,
                        height: 1040
                    }
                ),
                Bounds {
                    x: -1852,
                    y: -860,
                    width: 1280,
                    height: 800
                }
            );
        }

        #[test]
        fn optional_state_rejects_future_invalid_and_oversized_json() {
            let root =
                std::env::temp_dir().join(format!("nna-window-state-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            let path = root.join(FILE_NAME);
            assert!(read(&path).is_none());
            let mut saved = placement();
            std::fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();
            assert_eq!(read(&path), Some(saved.clone()));
            saved.version = 2;
            std::fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();
            assert!(read(&path).is_none());
            saved.version = 1;
            saved.display.scale_factor = 0.0;
            std::fs::write(&path, serde_json::to_vec(&saved).unwrap()).unwrap();
            assert!(read(&path).is_none());
            std::fs::write(&path, vec![b' '; MAX_STATE_BYTES as usize + 1]).unwrap();
            assert!(read(&path).is_none());
            std::fs::remove_file(path).unwrap();
            std::fs::remove_dir(root).unwrap();
        }

        #[test]
        fn queued_writer_uses_current_state_and_atomic_replace() {
            let root =
                std::env::temp_dir().join(format!("nna-window-write-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            let state = WindowState {
                path: root.join(FILE_NAME),
                saved: Arc::new(Mutex::new(Saved {
                    placement: Some(placement()),
                    written: None,
                    restoring: false,
                    pending: true,
                })),
            };
            let mut saved = state.saved.lock().unwrap();
            write(&state, &mut saved).unwrap();
            saved.placement.as_mut().unwrap().normal_bounds.x += 100;
            write(&state, &mut saved).unwrap();
            write(&state, &mut saved).unwrap();
            assert_eq!(read(&state.path), saved.placement);
            assert!(!state.path.with_extension("json.pending").exists());
            drop(saved);
            std::fs::remove_file(&state.path).unwrap();
            std::fs::remove_dir(root).unwrap();
        }
    }
}

pub(crate) fn initialize(app: &mut tauri::App, device_dir: &std::path::Path, visible: bool) {
    #[cfg(windows)]
    windows::initialize(app, device_dir, visible);
    #[cfg(not(windows))]
    if visible {
        use tauri::Manager;
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.show();
        }
    }
}

pub(crate) fn on_event(window: &tauri::Window, event: &tauri::WindowEvent) {
    #[cfg(windows)]
    windows::on_event(window, event);
}

pub(crate) async fn flush_app(app: &tauri::AppHandle) {
    #[cfg(windows)]
    windows::flush_app(app).await;
}
