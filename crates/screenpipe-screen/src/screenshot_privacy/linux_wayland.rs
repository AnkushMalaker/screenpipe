// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

//! KDE Wayland privacy captures use isolated compositor window surfaces, never
//! the portal's unfiltered monitor stream. A KWin-side transaction watches policy
//! changes throughout capture and validates the scene before releasing pixels.
//!
//! KWin's CaptureWindow renders the named window item, not a crop of the
//! workspace: https://invent.kde.org/plasma/kwin/-/blob/Plasma/6.8/src/plugins/screenshot/screenshot.cpp
//! Unsupported compositors or missing scene-change signals fail closed. The
//! compositor scene contains logical coordinates; each monitor's output maps
//! these to its own recording pixel dimensions (including mixed-scale setups).
use super::{scene, window_allowed, ExclusionSources, WindowFilters};
use crate::monitor::MonitorData;
use anyhow::{anyhow, ensure, Context, Result};
use image::{DynamicImage, Rgba, RgbaImage};
use serde::Deserialize;
use std::{
    collections::HashMap,
    io::{Read, Write},
    os::unix::net::UnixStream,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc, Arc, Mutex,
    },
    time::{Duration, Instant},
};
use zbus::{
    blocking::Connection,
    zvariant::{Fd, OwnedValue, Value},
};

static TRANSACTION_ID: AtomicU64 = AtomicU64::new(0);
const MAX_SURFACE_PIXELS: usize = 32 * 1024 * 1024;
const MAX_TRANSACTION_PIXELS: u64 = 128 * 1024 * 1024;

const PATH: &str = "/com/screenpipe/PrivacyCapture";
const SCRIPT: &str = include_str!("linux_wayland.js");

#[derive(Clone, Debug, Deserialize, PartialEq)]
struct Geometry {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}
#[derive(Clone, Debug, Deserialize, PartialEq)]
struct Output {
    name: String,
    rect: Geometry,
}
#[derive(Clone, Debug, Deserialize, PartialEq)]
struct Surface {
    id: String,
    app: String,
    title: String,
    pid: i32,
    rect: Geometry,
    opacity: f64,
    visible: bool,
    protected: bool,
}
#[derive(Clone, Debug, Deserialize, PartialEq)]
struct Scene {
    outputs: Vec<Output>,
    windows: Vec<Surface>,
}

type Pending = Option<Result<(Scene, DynamicImage)>>;
struct Transaction {
    monitor: MonitorData,
    filters: WindowFilters,
    sources: ExclusionSources,
    pending: Arc<Mutex<Pending>>,
    done: mpsc::Sender<Result<DynamicImage>>,
}
#[zbus::interface(name = "com.screenpipe.PrivacyCapture")]
impl Transaction {
    fn abort(&self, reason: &str) {
        let _ = self
            .done
            .send(Err(anyhow!("KWin privacy setup failed: {}", reason)));
    }
    async fn begin(&self, before: &str) {
        let before = before.to_owned();
        let monitor = self.monitor.clone();
        let filters = self.filters.clone();
        let sources = self.sources;
        let result = tokio::task::spawn_blocking(move || {
            ensure!(
                before.len() < 1024 * 1024,
                "KWin scene exceeds capture bound"
            );
            let scene: Scene = serde_json::from_str(&before).context("invalid KWin scene")?;
            let image = render(&scene, &monitor, &filters, sources)?;
            Ok((scene, image))
        })
        .await
        .unwrap_or_else(|error| Err(anyhow!("KWin capture worker failed: {error}")));
        *self.pending.lock().unwrap() = Some(result);
    }
    fn finish(&self, after: &str, changed: bool) {
        let result = (|| {
            ensure!(!changed, "KWin desktop changed during privacy capture");
            ensure!(
                after.len() < 1024 * 1024,
                "KWin scene exceeds capture bound"
            );
            let after: Scene = serde_json::from_str(after).context("invalid final KWin scene")?;
            let (before, image) = self
                .pending
                .lock()
                .unwrap()
                .take()
                .ok_or_else(|| anyhow!("KWin privacy capture did not start"))??;
            ensure!(
                before == after,
                "KWin desktop scene changed during privacy capture"
            );
            Ok(image)
        })();
        let _ = self.done.send(result);
    }
}

pub(super) fn capture(
    monitor: &MonitorData,
    filters: &WindowFilters,
    sources: ExclusionSources,
) -> Result<DynamicImage> {
    ensure!(
        std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .contains("kde"),
        "window-filtered Wayland screenshots require KDE isolated window capture"
    );
    let (send, recv) = mpsc::channel();
    let conn = zbus::blocking::connection::Builder::session()?
        .method_timeout(Duration::from_secs(2))
        .serve_at(
            PATH,
            Transaction {
                monitor: monitor.clone(),
                filters: filters.clone(),
                sources,
                pending: Arc::new(Mutex::new(None)),
                done: send,
            },
        )?
        .build()?;
    let destination = conn
        .unique_name()
        .ok_or_else(|| anyhow!("missing D-Bus capture identity"))?
        .as_str();
    let mut script = tempfile::Builder::new()
        .prefix("screenpipe-privacy-")
        .suffix(".js")
        .tempfile()?;
    write!(
        script,
        "var DESTINATION = {};\n{}",
        serde_json::to_string(destination)?,
        SCRIPT
    )?;
    script.flush()?;
    let name = format!(
        "screenpipe-privacy-{}-{}",
        std::process::id(),
        TRANSACTION_ID.fetch_add(1, Ordering::Relaxed)
    );
    let reply = conn.call_method(
        Some("org.kde.KWin"),
        "/Scripting",
        Some("org.kde.kwin.Scripting"),
        "loadScript",
        &(
            script.path().to_str().context("invalid script path")?,
            &name,
        ),
    )?;
    let id: i32 = reply.body().deserialize()?;
    ensure!(id >= 0, "KWin refused privacy script");
    let result = (|| {
        conn.call_method(
            Some("org.kde.KWin"),
            format!("/Scripting/Script{id}"),
            Some("org.kde.kwin.Script"),
            "run",
            &(),
        )?;
        recv.recv_timeout(Duration::from_secs(4))
            .context("KWin privacy transaction timed out")?
    })();
    let _ = conn.call_method(
        Some("org.kde.KWin"),
        "/Scripting",
        Some("org.kde.kwin.Scripting"),
        "unloadScript",
        &name,
    );
    result
}

fn valid_geometry(r: &Geometry) -> bool {
    [r.x, r.y, r.width, r.height].iter().all(|v| v.is_finite())
        && r.x.abs() <= f64::from(i32::MAX)
        && r.y.abs() <= f64::from(i32::MAX)
        && r.width > 0.0
        && r.height > 0.0
        && r.width <= 32768.0
        && r.height <= 32768.0
}
fn translated(r: &Geometry, output: &Geometry, sx: f64, sy: f64) -> scene::Rect {
    let x = ((r.x - output.x) * sx).floor() as i64;
    let y = ((r.y - output.y) * sy).floor() as i64;
    scene::Rect {
        x,
        y,
        width: (((r.x + r.width - output.x) * sx).ceil() as i64 - x).max(0) as u32,
        height: (((r.y + r.height - output.y) * sy).ceil() as i64 - y).max(0) as u32,
    }
}
fn render(
    scene: &Scene,
    monitor: &MonitorData,
    filters: &WindowFilters,
    sources: ExclusionSources,
) -> Result<DynamicImage> {
    let conn = zbus::blocking::connection::Builder::session()?
        .method_timeout(Duration::from_secs(1))
        .build()?;
    render_scene(scene, monitor, filters, sources, |id| {
        capture_surface(&conn, id)
    })
}
fn render_scene(
    scene: &Scene,
    monitor: &MonitorData,
    filters: &WindowFilters,
    sources: ExclusionSources,
    mut capture: impl FnMut(&str) -> Result<RgbaImage>,
) -> Result<DynamicImage> {
    ensure!(
        monitor.width > 0
            && monitor.height > 0
            && monitor.width <= 16384
            && monitor.height <= 16384
            && (monitor.width as usize * monitor.height as usize) <= MAX_SURFACE_PIXELS,
        "invalid capture dimensions"
    );
    let output = scene
        .outputs
        .iter()
        .find(|o| o.name == monitor.name)
        .or_else(|| {
            scene
                .outputs
                .iter()
                .find(|o| o.rect.x == f64::from(monitor.x) && o.rect.y == f64::from(monitor.y))
        })
        .ok_or_else(|| anyhow!("KWin output does not match recorder monitor"))?;
    ensure!(
        valid_geometry(&output.rect) && output.rect.width >= 1.0 && output.rect.height >= 1.0,
        "invalid KWin output geometry"
    );
    ensure!(scene.windows.len() <= 512, "too many KWin surfaces");
    let sx = f64::from(monitor.width) / output.rect.width;
    let sy = f64::from(monitor.height) / output.rect.height;
    let bounds = scene::Rect {
        x: 0,
        y: 0,
        width: monitor.width,
        height: monitor.height,
    };
    let mut canvas = RgbaImage::from_pixel(monitor.width, monitor.height, Rgba([0, 0, 0, 255]));
    let detector = sources
        .ignore_incognito_windows
        .then(|| screenpipe_a11y::incognito::create_detector(sources.enhanced_incognito_detection));
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut captured_pixels = 0u64;
    for window in &scene.windows {
        if !window.visible {
            continue;
        }
        ensure!(
            valid_geometry(&window.rect)
                && window.opacity.is_finite()
                && (0.0..=1.0).contains(&window.opacity),
            "invalid KWin window geometry"
        );
        let dest = translated(&window.rect, &output.rect, sx, sy);
        if !dest.intersects(bounds) || window.opacity == 0.0 {
            continue;
        }
        if window.protected
            || !window_allowed(
                filters,
                detector.as_deref(),
                &window.app,
                &window.title,
                window.pid,
            )
        {
            scene::paint_black(&mut canvas, dest);
            continue;
        }
        ensure!(
            Instant::now() < deadline,
            "KWin privacy capture exceeded time budget"
        );
        let destination_pixels = u64::from(dest.width) * u64::from(dest.height);
        ensure!(
            destination_pixels <= MAX_SURFACE_PIXELS as u64,
            "KWin scaled surface exceeds pixel budget"
        );
        captured_pixels += destination_pixels;
        ensure!(
            captured_pixels <= MAX_TRANSACTION_PIXELS,
            "KWin privacy scene exceeds pixel budget"
        );
        let mut surface = capture(&window.id)?;
        if surface.width() != dest.width || surface.height() != dest.height {
            surface = image::imageops::resize(
                &surface,
                dest.width,
                dest.height,
                image::imageops::FilterType::Triangle,
            );
        }
        // CaptureWindow has already applied WindowItem.opacity to its
        // premultiplied pixels. Multiplying again would darken transparency.
        scene::overlay_premultiplied(&mut canvas, &surface, dest.x, dest.y, None);
    }
    ensure!(
        Instant::now() < deadline,
        "KWin privacy capture exceeded time budget"
    );
    Ok(DynamicImage::ImageRgba8(canvas))
}
fn capture_surface(conn: &Connection, id: &str) -> Result<RgbaImage> {
    let (mut read, write) = UnixStream::pair()?;
    read.set_read_timeout(Some(Duration::from_secs(2)))?;
    let options: HashMap<&str, Value<'_>> = HashMap::from([
        ("include-decoration", Value::Bool(true)),
        ("include-shadow", Value::Bool(false)),
        ("include-cursor", Value::Bool(false)),
    ]);
    let reply = conn.call_method(
        Some("org.kde.KWin"),
        "/org/kde/KWin/ScreenShot2",
        Some("org.kde.KWin.ScreenShot2"),
        "CaptureWindow",
        &(id, options, Fd::from(&write)),
    )?;
    let meta: HashMap<String, OwnedValue> = reply.body().deserialize()?;
    let number = |name: &str| -> Result<u32> {
        Ok(u32::try_from(
            meta.get(name)
                .ok_or_else(|| anyhow!("missing KWin image metadata"))?,
        )?)
    };
    let width = number("width")?;
    let height = number("height")?;
    ensure!(
        width > 0 && height > 0 && width <= 16384 && height <= 16384,
        "invalid KWin image dimensions"
    );
    ensure!(
        number("format")? == 18,
        "unsupported KWin isolated pixel format"
    );
    ensure!(
        <&str>::try_from(
            meta.get("windowId")
                .ok_or_else(|| anyhow!("missing captured window identity"))?
        )? == id,
        "KWin captured a different surface"
    );
    let pixels = width as usize * height as usize;
    ensure!(
        pixels <= MAX_SURFACE_PIXELS,
        "KWin surface exceeds pixel budget"
    );
    let mut bytes = vec![0; pixels * 4];
    read.read_exact(&mut bytes)
        .context("incomplete KWin surface")?;
    RgbaImage::from_raw(width, height, bytes).ok_or_else(|| anyhow!("invalid KWin pixel buffer"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_premultiplied_opacity_is_applied_exactly_once() {
        let monitor = MonitorData {
            width: 1,
            height: 1,
            x: 0,
            y: 0,
            name: "test".into(),
            is_primary: true,
        };
        let geometry = Geometry {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        };
        let scene = Scene {
            outputs: vec![Output {
                name: "test".into(),
                rect: geometry.clone(),
            }],
            windows: vec![Surface {
                id: "allowed".into(),
                app: "Editor".into(),
                title: "public".into(),
                pid: 1,
                rect: geometry,
                opacity: 0.5,
                visible: true,
                protected: false,
            }],
        };
        let image = render_scene(
            &scene,
            &monitor,
            &WindowFilters::new(&[], &[], &[]),
            ExclusionSources::default(),
            |_| Ok(RgbaImage::from_pixel(1, 1, Rgba([0, 128, 128, 128]))),
        )
        .unwrap()
        .to_rgba8();
        assert_eq!(image.get_pixel(0, 0).0, [0, 128, 128, 255]);
    }
    #[test]
    fn excludes_pixels_at_source_and_preserves_allowed_foreground() {
        let monitor = MonitorData {
            width: 4,
            height: 4,
            x: 0,
            y: 0,
            name: "test".into(),
            is_primary: true,
        };
        let mut windows = Vec::new();
        for (id, title, x, y, w, h) in [
            ("back", "public", 0.0, 0.0, 4.0, 4.0),
            ("private", "Secret", 1.0, 1.0, 2.0, 2.0),
            ("front", "public", 2.0, 2.0, 1.0, 1.0),
        ] {
            windows.push(Surface {
                id: id.into(),
                app: "Editor".into(),
                title: title.into(),
                pid: 1,
                rect: Geometry {
                    x,
                    y,
                    width: w,
                    height: h,
                },
                opacity: 1.0,
                visible: true,
                protected: false,
            });
        }
        let scene = Scene {
            outputs: vec![Output {
                name: "test".into(),
                rect: Geometry {
                    x: 0.0,
                    y: 0.0,
                    width: 4.0,
                    height: 4.0,
                },
            }],
            windows,
        };
        let mut captured = Vec::new();
        let pixels = render_scene(
            &scene,
            &monitor,
            &WindowFilters::new(&["::Secret".into()], &[], &[]),
            ExclusionSources::default(),
            |id| {
                captured.push(id.to_string());
                match id {
                    "back" => Ok(RgbaImage::from_pixel(4, 4, Rgba([0, 200, 0, 255]))),
                    "front" => Ok(RgbaImage::from_pixel(1, 1, Rgba([0, 255, 255, 255]))),
                    _ => panic!("excluded surface must never be requested"),
                }
            },
        )
        .unwrap()
        .to_rgba8();
        assert_eq!(captured, vec!["back", "front"]);
        assert_eq!(pixels.get_pixel(0, 0).0, [0, 200, 0, 255]);
        assert_eq!(pixels.get_pixel(1, 1).0, [0, 0, 0, 255]);
        assert_eq!(pixels.get_pixel(2, 2).0, [0, 255, 255, 255]);
    }
    #[test]
    fn fractional_bounds_cover_every_excluded_pixel() {
        let out = Geometry {
            x: -1920.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
        };
        let r = Geometry {
            x: -1919.75,
            y: 0.25,
            width: 10.5,
            height: 10.5,
        };
        assert_eq!(
            translated(&r, &out, 1.5, 1.5),
            scene::Rect {
                x: 0,
                y: 0,
                width: 17,
                height: 17
            }
        );
    }
    #[test]
    fn invalid_native_geometry_is_rejected() {
        for width in [0.0, -1.0, f64::NAN, f64::INFINITY, 32769.0] {
            assert!(!valid_geometry(&Geometry {
                x: 0.0,
                y: 0.0,
                width,
                height: 10.0
            }));
        }
    }
    #[test]
    fn transaction_rejects_aba_even_with_matching_final_scene() {
        let (send, recv) = mpsc::channel();
        let t = Transaction {
            monitor: MonitorData {
                width: 1,
                height: 1,
                x: 0,
                y: 0,
                name: "test".into(),
                is_primary: true,
            },
            filters: WindowFilters::new(&[], &[], &[]),
            sources: ExclusionSources::default(),
            pending: Arc::new(Mutex::new(None)),
            done: send,
        };
        t.finish("{}", true);
        assert!(recv
            .recv()
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("changed"));
    }
}
