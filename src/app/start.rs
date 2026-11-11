use super::*;

pub fn run(receiver: Receiver<Msg>, sender: Sender<Msg>, width: u32, height: u32, start: Start) {
    remote::register(platform::window_no(), sender.clone());
    remote::start();
    crate::api::start();
    let mut app = match App::new(sender, width, height, start) {
        Ok(app) => app,
        Err(error) => {
            crate::fatal(&error);
            platform::quit();
            return;
        }
    };
    crate::trace("app: event loop");
    loop {
        let message = match app.next_wakeup() {
            Some(at) => receiver.recv_timeout(at.saturating_duration_since(Instant::now())),
            None => receiver.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match message {
            Ok(message) => {
                crate::trace(match &message {
                    Msg::Ui(_) => "loop: ui event",
                    Msg::Pty(..) => "loop: pty data",
                    Msg::ShellExited(..) => "loop: shell exited",
                    Msg::Notice(..) => "loop: notice",
                    Msg::Update(..) => "loop: update",
                    Msg::Git(..) => "loop: git",
                    Msg::Viewer(..) => "loop: viewer",
                    Msg::Remote(..) => "loop: remote",
                    Msg::Api(..) => "loop: api",
                });
                app.handle(message)
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        // Coalesce everything already queued into one frame, within reason.
        let budget = Instant::now() + Duration::from_millis(12);
        while Instant::now() < budget {
            match receiver.try_recv() {
                Ok(message) => app.handle(message),
                Err(_) => break,
            }
        }
        app.tick();
        if app.quitting {
            break;
        }
        if app.dirty {
            app.render();
        }
    }
    remote::unregister(platform::window_no());
    let leave = app.leave.take();
    drop(app);
    if let Some(leave) = leave {
        leave();
    }
}

/// Renders one frame without a window and writes it as a PPM: checks the
/// Vulkan path on its own. Returns the process exit code.
pub fn selftest(path: &str, width: u32, height: u32, frames: usize) -> i32 {
    let started = Instant::now();
    let mut gpu = match Gpu::new() {
        Ok(gpu) => gpu,
        Err(error) => {
            eprintln!("selftest: {error}");
            return 1;
        }
    };
    println!("device: {} ({:.1} ms)", gpu.device_name, started.elapsed().as_secs_f64() * 1000.0);
    let mut text = TextSystem::new(BASE_FONT_SIZE);
    let theme = Theme::dark();
    let mut scene = Scene::new();
    let mut slowest = 0.0f64;
    for round in 0..frames {
        let frame = Instant::now();
        scene.reset(width as f32, height as f32, 1.0, theme.background);
        let card = Rect::new(16.0, 16.0, 448.0, 128.0);
        scene.shadow(card, theme.shadow(0.6), 10.0, 8.0);
        scene.quad(Quad::new(card, theme.surface).radius(10.0).border(1.0, theme.divider));
        text.draw_text(&mut scene, "Temino self-test", 32.0, 52.0, UI_BOLD, 20.0, theme.text);
        text.draw_mono(
            &mut scene,
            "ls -la ~/config ─┼─ ✓ 中文 🌞🐱👌🚀🔥🎉",
            32.0,
            70.0,
            Style::default(),
            theme.success,
        );
        scene.fill(Rect::new(32.0, 110.0, 120.0, 18.0), theme.accent);
        match gpu.render(&scene, &mut text.atlas, &mut text.color_atlas, true) {
            Ok(Some(pixels)) => {
                let ms = frame.elapsed().as_secs_f64() * 1000.0;
                slowest = if round > 0 { slowest.max(ms) } else { slowest };
                if round < 3 || round % 100 == 0 {
                    println!("frame {round}: {ms:.2} ms");
                }
                if round + 1 == frames {
                    println!("{frames} frames, slowest after the first {slowest:.2} ms");
                    write_ppm(std::ffi::OsStr::new(path), pixels, width, height);
                    println!("wrote {path}");
                }
            }
            Ok(None) => unreachable!("a readback always returns the frame"),
            Err(error) => {
                eprintln!("selftest: {error}");
                return 1;
            }
        }
    }
    0
}

pub fn host_name() -> Option<String> {
    #[cfg(not(target_os = "haiku"))]
    {
        let mut buffer = [0u8; 256];
        if unsafe { libc::gethostname(buffer.as_mut_ptr() as *mut libc::c_char, buffer.len()) } == 0 {
            let end = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
            let name = String::from_utf8_lossy(&buffer[..end]).into_owned();
            return Some(name.trim_end_matches(".local").to_owned());
        }
    }
    std::fs::read_to_string("/boot/system/settings/network/hostname").ok()
}

pub(super) fn user_host() -> String {
    let user = std::env::var("USER")
        .ok()
        .filter(|u| !u.is_empty())
        .or_else(platform::user_name)
        .unwrap_or_else(|| "user".into());
    let host = host_name()
        .map(|h| h.trim().to_owned())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| platform::OS_NAME.to_lowercase());
    format!("{user}@{host}")
}

/// `background=<name>` in the settings file, as an index into BACKGROUNDS
/// (None, the theme's own, when unset or unknown).
pub(super) fn load_background() -> Option<usize> {
    crate::setting("background").and_then(|name| BACKGROUNDS.iter().position(|(n, _)| *n == name))
}

/// `line_spacing=<name>` in the settings file, as a multiple of the font's
/// line height (the default's when unset or unknown).
pub(super) fn load_line_spacing() -> f32 {
    let name = crate::setting("line_spacing").unwrap_or_default();
    let found = crate::text::LINE_SPACINGS.iter().find(|(n, _)| n.eq_ignore_ascii_case(&name));
    found.unwrap_or(&crate::text::LINE_SPACINGS[1]).1
}

/// Where theme files go: Ghostty's format, one scheme per file.
pub fn themes_dir() -> String {
    format!("{}/themes", crate::settings_dir())
}

/// `opacity=<percent>` in the settings file, as an index into OPACITIES
/// (fully opaque when unset, unknown, or off macOS, whose windows are the
/// only ones that show through).
pub(super) fn load_opacity() -> usize {
    crate::setting("opacity")
        .filter(|_| cfg!(target_os = "macos"))
        .and_then(|value| OPACITIES.iter().position(|o| o.to_string() == value))
        .unwrap_or(0)
}

pub(super) fn write_ppm(path: &std::ffi::OsStr, bgra: &[u8], width: u32, height: u32) {
    let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
    ppm.reserve(bgra.len() / 4 * 3);
    for pixel in bgra.as_chunks::<4>().0 {
        ppm.extend_from_slice(&[pixel[2], pixel[1], pixel[0]]);
    }
    let _ = std::fs::write(path, ppm);
}
