use super::*;
use crate::Locked;

pub(super) const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// One open window: its winit window, how frames reach it, its core, and
/// the pointer and keyboard state its events are made from.
pub(super) struct Win {
    pub(super) no: u32,
    pub(super) window: Arc<Window>,
    pub(super) surface: Option<softbuffer::Surface<Arc<Window>, Arc<Window>>>,
    /// Frames go straight to the window's Metal layer, not through softbuffer.
    #[cfg(target_os = "macos")]
    pub(super) on_layer: bool,
    pub(super) sender: Sender<Msg>,
    pub(super) scale: f64,
    pub(super) modifiers: ModifiersState,
    /// The pointer, in logical pixels.
    pub(super) pointer: (f32, f32),
    pub(super) buttons: i32,
    pub(super) last_click: Option<(Instant, (f32, f32), i32)>,
    pub(super) clicks: i32,
    /// An input method is composing: its keys aren't ours.
    pub(super) composing: bool,
    /// Super or Ctrl, as their own key presses and releases say.
    pub(super) chord_down: bool,
    /// A Super or Ctrl release came with no press before it. ibus over XIM
    /// does that at the start of a chord: a release, the chord's letter with
    /// nothing held, then the press and the letter again, properly held.
    pub(super) orphan_chord: bool,
}

pub(super) struct Handler {
    pub(super) title: String,
    pub(super) size: (u32, u32),
    /// The first window's core, which main started, until its window opens.
    pub(super) first: Option<Sender<Msg>>,
    pub(super) windows: Vec<Win>,
    /// The next window's number.
    pub(super) next: u32,
    /// The window that keeps the tabs for the next launch: the first, then
    /// the oldest left once it closes.
    pub(super) primary: u32,
}

impl Win {
    pub(super) fn send(&self, event: UiEvent) {
        let _ = self.sender.send(Msg::Ui(event));
    }

    /// Remembers where the window is and how big, for the next launch.
    pub(super) fn save_frame(&self) {
        let window = &self.window;
        if window.is_minimized() == Some(true) || window.fullscreen().is_some() {
            return;
        }
        // winit on macOS places a new window by its content, not its frame.
        let position = if cfg!(target_os = "macos") { window.inner_position() } else { window.outer_position() };
        let Ok(position) = position else { return };
        let position = position.to_logical::<f64>(self.scale);
        let size = window.inner_size().to_logical::<f64>(self.scale);
        let frame = format!("{:.0},{:.0},{:.0},{:.0}", position.x, position.y, size.width, size.height);
        if crate::setting("window").as_deref() != Some(frame.as_str()) {
            crate::save_setting("window", &frame);
        }
    }

    pub(super) fn send_size(&self) {
        let size = self.window.inner_size();
        let scale = self.scale;
        self.send(UiEvent::Resize {
            width: ((size.width as f64 / scale).round() as u32).max(1),
            height: ((size.height as f64 / scale).round() as u32).max(1),
            scale: scale as f32,
        });
    }

    pub(super) fn mods(&self) -> u32 {
        let m = self.modifiers;
        let mut bits = 0;
        if m.shift_key() {
            bits |= mods::SHIFT;
        }
        if m.control_key() {
            bits |= mods::CONTROL;
        }
        if m.alt_key() {
            bits |= mods::OPTION;
        }
        if m.super_key() {
            bits |= mods::COMMAND;
        }
        bits
    }

    pub(super) fn draw(&mut self) {
        let mut frames = FRAMES.locked();
        let Some((_, frame, pending)) = frames.iter_mut().find(|(no, ..)| *no == self.no) else { return };
        *pending = false;
        #[cfg(target_os = "macos")]
        if self.on_layer {
            return;
        }
        let Some(surface) = &mut self.surface else { return };
        let (Some(width), Some(height)) = (NonZeroU32::new(frame.width), NonZeroU32::new(frame.height)) else {
            return;
        };
        if surface.resize(width, height).is_err() {
            return;
        }
        if let Ok(mut buffer) = surface.buffer_mut() {
            if buffer.len() == frame.pixels.len() {
                buffer.copy_from_slice(&frame.pixels);
                let _ = buffer.present();
            }
        }
    }

    pub(super) fn key(&self, event: &WinitKey) {
        let named = match &event.logical_key {
            Key::Named(key) => match key {
                NamedKey::ArrowUp => Some(Named::Up),
                NamedKey::ArrowDown => Some(Named::Down),
                NamedKey::ArrowLeft => Some(Named::Left),
                NamedKey::ArrowRight => Some(Named::Right),
                NamedKey::Home => Some(Named::Home),
                NamedKey::End => Some(Named::End),
                NamedKey::PageUp => Some(Named::PageUp),
                NamedKey::PageDown => Some(Named::PageDown),
                NamedKey::Insert => Some(Named::Insert),
                NamedKey::Delete => Some(Named::Delete),
                NamedKey::Backspace => Some(Named::Backspace),
                NamedKey::Tab => Some(Named::Tab),
                NamedKey::Enter => Some(Named::Enter),
                NamedKey::Escape => Some(Named::Escape),
                NamedKey::F1 => Some(Named::F(1)),
                NamedKey::F2 => Some(Named::F(2)),
                NamedKey::F3 => Some(Named::F(3)),
                NamedKey::F4 => Some(Named::F(4)),
                NamedKey::F5 => Some(Named::F(5)),
                NamedKey::F6 => Some(Named::F(6)),
                NamedKey::F7 => Some(Named::F(7)),
                NamedKey::F8 => Some(Named::F(8)),
                NamedKey::F9 => Some(Named::F(9)),
                NamedKey::F10 => Some(Named::F(10)),
                NamedKey::F11 => Some(Named::F(11)),
                NamedKey::F12 => Some(Named::F(12)),
                NamedKey::Space => None,
                // Modifiers and the like produce nothing.
                _ => return,
            },
            Key::Character(_) => None,
            _ => return,
        };
        let base = match event.key_without_modifiers() {
            Key::Character(text) => text.chars().next(),
            Key::Named(NamedKey::Space) => Some(' '),
            _ => None,
        };
        let mut bits = self.mods();
        let mut bytes = event.text_with_all_modifiers().map(|t| t.as_bytes().to_vec()).unwrap_or_default();
        if bytes.is_empty() {
            if let Some(c) = base.filter(|_| bits & mods::CONTROL != 0) {
                bytes = c.to_string().into_bytes();
            }
        }
        if cfg!(target_os = "macos") && bits & mods::OPTION != 0 {
            // Option is Meta: the key itself, not the character macOS composes.
            if let Some(c) = base.filter(|c| c.is_ascii_graphic()) {
                let c = if bits & mods::SHIFT != 0 { c.to_ascii_uppercase() } else { c };
                bytes = c.to_string().into_bytes();
            }
        }
        if cfg!(target_os = "linux")
            && bits & mods::CONTROL != 0
            && bits & mods::SHIFT != 0
            && named != Some(Named::Tab)
        {
            // Ctrl+Shift is the shortcut modifier, as in other Linux terminals.
            bits = (bits & !(mods::CONTROL | mods::SHIFT)) | mods::COMMAND;
        }
        if cfg!(target_os = "linux") && bits == mods::OPTION && base.is_some_and(|c| ('1'..='9').contains(&c)) {
            // Alt+1…9 picks a tab, as in GNOME Terminal: Super+1…9 is the dock's.
            bits = mods::COMMAND;
        }
        self.send(UiEvent::Key(KeyEvent { named, mods: bits, raw_char: base.map_or(0, |c| c as u32), bytes }));
    }
}

impl Handler {
    pub(super) fn win(&mut self, no: u32) -> Option<&mut Win> {
        self.windows.iter_mut().find(|win| win.no == no)
    }

    /// Opens a window: the first one where the last launch left it, with the
    /// core main started; another cascading from window `from`, with a new
    /// core whose first tab opens in `cwd`.
    pub(super) fn open_window(&mut self, event_loop: &ActiveEventLoop, from: Option<u32>, cwd: Option<String>) {
        let no = self.next;
        let mut attributes = Window::default_attributes()
            .with_title(self.title.clone())
            .with_inner_size(LogicalSize::new(self.size.0, self.size.1))
            // Settings ▸ Background Opacity lets the desktop show through.
            .with_transparent(cfg!(target_os = "macos"));
        // The window's content runs up under the title bar, which Temino
        // draws itself at TITLEBAR points.
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::WindowAttributesExtMacOS;
            attributes =
                attributes.with_titlebar_transparent(true).with_fullsize_content_view(true).with_title_hidden(true);
        }
        let parent = from.and_then(|from| self.windows.iter().find(|win| win.no == from));
        let place = match parent {
            Some(parent) => {
                // A little down and right of the window it came from, its size.
                let window = &parent.window;
                let position =
                    if cfg!(target_os = "macos") { window.inner_position() } else { window.outer_position() };
                let size = window.inner_size().to_logical::<f64>(parent.scale);
                position.ok().map(|p| {
                    let p = p.to_logical::<f64>(parent.scale);
                    (p.x + CASCADE, p.y + CASCADE, size.width, size.height)
                })
            }
            None => saved_frame(),
        };
        if let Some((x, y, width, height)) = place {
            attributes =
                attributes.with_position(LogicalPosition::new(x, y)).with_inner_size(LogicalSize::new(width, height));
        }
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                crate::fatal(&format!("opening the window: {error}"));
                if self.windows.is_empty() {
                    event_loop.exit();
                }
                return;
            }
        };
        self.next += 1;
        window.set_cursor(CursorIcon::Text);
        // Pinyin and the other input methods.
        window.set_ime_allowed(true);
        #[cfg(not(target_os = "macos"))]
        window.set_blur(BLUR.load(Ordering::Acquire));
        #[cfg(target_os = "macos")]
        if self.windows.is_empty() && self.first.is_some() {
            menu::install();
        }
        let (sender, receiver) = match self.first.take() {
            Some(sender) => (sender, None),
            None => {
                let (sender, receiver) = std::sync::mpsc::channel();
                (sender, Some(receiver))
            }
        };
        let mut win = Win {
            no,
            window: window.clone(),
            surface: None,
            #[cfg(target_os = "macos")]
            on_layer: false,
            sender: sender.clone(),
            scale: window.scale_factor(),
            modifiers: ModifiersState::empty(),
            pointer: (0.0, 0.0),
            buttons: 0,
            last_click: None,
            clicks: 0,
            composing: false,
            chord_down: false,
            orphan_chord: false,
        };
        #[cfg(target_os = "macos")]
        {
            use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
            if let Ok(RawWindowHandle::AppKit(handle)) = window.window_handle().map(|h| h.as_raw()) {
                unsafe {
                    temino_metal_attach(handle.ns_view.as_ptr(), no);
                    temino_titlebar(handle.ns_view.as_ptr(), TITLEBAR as f64);
                    temino_blur(handle.ns_view.as_ptr(), BLUR.load(Ordering::Acquire));
                    temino_viewer_init(handle.ns_view.as_ptr(), no, viewer_event);
                }
                win.on_layer = true;
            }
        }
        #[cfg(target_os = "macos")]
        let needs_surface = !win.on_layer;
        #[cfg(not(target_os = "macos"))]
        let needs_surface = true;
        if needs_surface {
            let surface = softbuffer::Context::new(window.clone())
                .and_then(|context| softbuffer::Surface::new(&context, window.clone()));
            match surface {
                Ok(surface) => win.surface = Some(surface),
                Err(error) => {
                    crate::fatal(&format!("setting up the window surface: {error}"));
                    if self.windows.is_empty() {
                        event_loop.exit();
                    }
                    return;
                }
            }
        }
        crate::trace(&format!("window {no}: opened"));
        SENDERS.locked().push((no, sender.clone()));
        win.send_size();
        self.windows.push(win);
        WINDOWS.store(self.windows.len(), Ordering::Release);
        if let (Some(receiver), Some(start)) = (receiver, NEW_CORE.get().copied()) {
            let started = std::thread::Builder::new().name(format!("temino core {no}")).spawn(move || {
                WINDOW.with(|window| window.set(no));
                start(receiver, sender, cwd);
            });
            if started.is_err() {
                self.close_window(event_loop, no);
            }
        }
    }

    /// Closes window `no` (its core is done with it); the oldest window
    /// left takes over keeping the tabs, and with none left the app quits.
    pub(super) fn close_window(&mut self, event_loop: &ActiveEventLoop, no: u32) {
        let Some(index) = self.windows.iter().position(|win| win.no == no) else { return };
        let win = self.windows.remove(index);
        #[cfg(target_os = "macos")]
        unsafe {
            temino_viewer_detach(no);
            temino_metal_detach(no);
        }
        FRAMES.locked().retain(|(n, ..)| *n != no);
        SENDERS.locked().retain(|(n, _)| *n != no);
        drop(win);
        WINDOWS.store(self.windows.len(), Ordering::Release);
        crate::trace(&format!("window {no}: closed, {} left", self.windows.len()));
        let Some(next) = self.windows.first() else {
            event_loop.exit();
            return;
        };
        if no == self.primary {
            self.primary = next.no;
            next.send(UiEvent::Primary);
        }
    }
}

/// How far down and right a new window opens from the one it came from.
pub(super) const CASCADE: f64 = 26.0;

impl ApplicationHandler<(u32, Request)> for Handler {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.first.is_some() && self.windows.is_empty() {
            self.open_window(event_loop, None, None);
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, (no, request): (u32, Request)) {
        match request {
            Request::Present => {
                if let Some(win) = self.win(no) {
                    win.draw();
                }
            }
            Request::Title(title) => {
                if let Some(win) = self.win(no) {
                    win.window.set_title(&title);
                }
            }
            Request::Cursor(cursor) => {
                if let Some(win) = self.win(no) {
                    win.window.set_cursor(match cursor {
                        Cursor::IBeam => CursorIcon::Text,
                        Cursor::Arrow => CursorIcon::Default,
                        Cursor::ResizeColumn => CursorIcon::ColResize,
                        Cursor::ResizeRow => CursorIcon::RowResize,
                        Cursor::Link => CursorIcon::Pointer,
                    });
                }
            }
            Request::Blur(on) => {
                if let Some(win) = self.win(no) {
                    #[cfg(not(target_os = "macos"))]
                    win.window.set_blur(on);
                    #[cfg(target_os = "macos")]
                    {
                        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
                        if let Ok(RawWindowHandle::AppKit(handle)) = win.window.window_handle().map(|h| h.as_raw()) {
                            unsafe { temino_blur(handle.ns_view.as_ptr(), on) };
                        }
                    }
                }
            }
            Request::Caret(x, y, height) => {
                if let Some(win) = self.win(no) {
                    // X11's XIM only takes a spot, the baseline an input
                    // method draws its candidates under (winit drops the
                    // size there), so give it the caret's bottom; from the
                    // top the candidate window covers the line.
                    #[cfg(all(unix, not(target_os = "macos")))]
                    let y = {
                        use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
                        let x11 = matches!(
                            win.window.window_handle().map(|h| h.as_raw()),
                            Ok(RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_))
                        );
                        if x11 {
                            y + height.max(1)
                        } else {
                            y
                        }
                    };
                    win.window.set_ime_cursor_area(LogicalPosition::new(x, y), LogicalSize::new(1, height.max(1)));
                }
            }
            #[cfg(target_os = "macos")]
            Request::ViewerOpen(id, kind, target, style) => {
                let target = std::ffi::CString::new(target.replace('\0', "")).unwrap_or_default();
                unsafe { temino_viewer_open(id, kind as i32, target.as_ptr(), &style) };
            }
            #[cfg(target_os = "macos")]
            Request::ViewerPlace(frames) => {
                for (id, [x, y, w, h], visible) in frames {
                    unsafe { temino_viewer_place(id, x as f64, y as f64, w as f64, h as f64, visible) };
                }
            }
            #[cfg(target_os = "macos")]
            Request::ViewerClose(id) => unsafe { temino_viewer_close(id) },
            #[cfg(target_os = "macos")]
            Request::ViewerGo(id, address) => {
                let address = std::ffi::CString::new(address.replace('\0', "")).unwrap_or_default();
                unsafe { temino_viewer_go(id, address.as_ptr()) };
            }
            #[cfg(target_os = "macos")]
            Request::ViewerFocus(id) => unsafe { temino_viewer_focus(id.unwrap_or(0)) },
            #[cfg(not(target_os = "macos"))]
            Request::ViewerOpen(..)
            | Request::ViewerPlace(_)
            | Request::ViewerClose(_)
            | Request::ViewerFocus(_)
            | Request::ViewerGo(..) => {}
            Request::NewWindow(cwd) => self.open_window(event_loop, Some(no), cwd),
            Request::CloseWindow => self.close_window(event_loop, no),
            Request::Quit => {
                // The others are asked to finish too (one with unsaved work
                // stays open), then this one closes.
                for win in self.windows.iter().filter(|win| win.no != no) {
                    win.send(UiEvent::Quit);
                }
                self.close_window(event_loop, no);
            }
            #[cfg(target_os = "macos")]
            Request::Hotkey(key) => {
                let (code, modifiers) = key.unwrap_or((u32::MAX, 0));
                unsafe { temino_hotkey(code, modifiers) }
            }
            #[cfg(target_os = "macos")]
            Request::ToggleWindow => unsafe { temino_toggle_window() },
            #[cfg(not(target_os = "macos"))]
            Request::Hotkey(_) | Request::ToggleWindow => {}
        }
    }

    fn window_event(&mut self, _event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let primary = self.primary;
        let Some(win) = self.windows.iter_mut().find(|win| win.window.id() == id) else { return };
        match event {
            WindowEvent::CloseRequested => win.send(UiEvent::Close),
            WindowEvent::Resized(_) => {
                win.send_size();
                if win.no == primary {
                    win.save_frame();
                }
            }
            WindowEvent::Moved(_) => {
                if win.no == primary {
                    win.save_frame();
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                win.scale = scale_factor;
                win.send_size();
            }
            WindowEvent::RedrawRequested => win.draw(),
            WindowEvent::Focused(focused) => {
                if focused {
                    // Menu picks go to the window in front.
                    *MENU_SENDER.locked() = Some(win.sender.clone());
                }
                win.send(UiEvent::Focus(focused));
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                win.modifiers = modifiers.state();
                win.send(UiEvent::Modifiers(win.mods()));
            }
            WindowEvent::Ime(ime) => match ime {
                Ime::Preedit(text, _) => {
                    win.composing = !text.is_empty();
                    win.send(UiEvent::Preedit(text));
                }
                Ime::Commit(text) => {
                    // Linux input methods also commit the letter of a
                    // shortcut (Super+B, Ctrl+Shift+B) the key event already
                    // ran; only a composition's commit is text.
                    let shortcut = win.modifiers.super_key() || win.modifiers.control_key();
                    let composed = std::mem::replace(&mut win.composing, false);
                    if composed || !shortcut {
                        win.send(UiEvent::Text(text));
                    }
                }
                Ime::Disabled => {
                    win.composing = false;
                    win.send(UiEvent::Preedit(String::new()));
                }
                Ime::Enabled => {}
            },
            WindowEvent::KeyboardInput { event, .. } => {
                let chord = matches!(event.logical_key, Key::Named(NamedKey::Super | NamedKey::Control));
                let mut ghost = false;
                match (chord, event.state) {
                    (true, ElementState::Pressed) => {
                        win.chord_down = true;
                        win.orphan_chord = false;
                    }
                    (true, ElementState::Released) => {
                        win.orphan_chord = !win.chord_down;
                        win.chord_down = false;
                    }
                    (false, ElementState::Pressed) => {
                        // The unheld copy of a chord's letter; the held one follows.
                        ghost = std::mem::take(&mut win.orphan_chord)
                            && matches!(event.logical_key, Key::Character(_))
                            && win.modifiers.is_empty();
                    }
                    (false, ElementState::Released) => {}
                }
                if event.state == ElementState::Pressed && !win.composing && !ghost {
                    win.key(&event);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                win.pointer = ((position.x / win.scale) as f32, (position.y / win.scale) as f32);
                let (x, y) = win.pointer;
                win.send(UiEvent::MouseMoved { x, y, buttons: win.buttons });
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let bit = match button {
                    MouseButton::Left => buttons::PRIMARY,
                    MouseButton::Right => buttons::SECONDARY,
                    MouseButton::Middle => buttons::TERTIARY,
                    _ => return,
                };
                let (x, y) = win.pointer;
                if state == ElementState::Pressed {
                    let now = Instant::now();
                    let repeat = win.last_click.is_some_and(|(at, (px, py), b)| {
                        b == bit
                            && now.duration_since(at) < DOUBLE_CLICK
                            && (px - x).abs() < 4.0
                            && (py - y).abs() < 4.0
                    });
                    win.clicks = if repeat { win.clicks + 1 } else { 1 };
                    win.last_click = Some((now, (x, y), bit));
                    win.buttons |= bit;
                    win.send(UiEvent::MouseDown { x, y, buttons: bit, clicks: win.clicks, mods: win.mods() });
                } else {
                    win.buttons &= !bit;
                    win.send(UiEvent::MouseUp { x, y });
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Positive dy scrolls down, in lines, as on Haiku.
                let (dx, dy, precise) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (-x, -y, false),
                    MouseScrollDelta::PixelDelta(PhysicalPosition { x, y }) => {
                        let line = 16.0 * win.scale;
                        ((-x / line) as f32 / 3.0, (-y / line) as f32 / 3.0, true)
                    }
                };
                let (x, y) = win.pointer;
                win.send(UiEvent::Wheel { dx, dy, x, y, mods: win.mods(), precise });
            }
            _ => {}
        }
    }
}
