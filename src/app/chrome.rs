use super::*;

/// A tab's background, in the sidebar or the strip: a tab with a color is
/// tinted with it all over, lightly, with the color whole in a rail down
/// its leading edge; the current tab is raised and drawn round with
/// a clear border, in its color if it has one. Any other tab lifts a
/// little under the pointer.
pub(super) fn draw_tab_background(
    scene: &mut Scene,
    theme: &Theme,
    rect: Rect,
    color: Option<Color>,
    current: bool,
    dragged: bool,
    hovered: bool,
) {
    let (fill, border) = match (color, current) {
        (Some(color), true) => (theme.raised.mix(color, 0.12), color.mix(theme.text, 0.15)),
        (Some(color), false) => (color.alpha(if hovered || dragged { 0.17 } else { 0.1 }), Color::TRANSPARENT),
        (None, true) => (theme.raised.mix(theme.text, 0.06), theme.text_dim),
        (None, false) if hovered || dragged => (theme.panel(HOVER_LIFT), Color::TRANSPARENT),
        (None, false) => return,
    };
    let fill = if dragged { fill.mix(theme.text, 0.06) } else { fill };
    let width = if current { 1.5 } else { 1.0 };
    scene.quad(Quad::new(rect, fill).radius(6.0).border(width, border));
    // The color itself, whole, in a rail down the leading edge: the tint
    // alone turns muddy on a dark background.
    if let Some(color) = color {
        // Inset as on a one-line row, however tall: under a row grown by
        // its pane lines the rail runs the whole height, not a short dash.
        let inset = (rect.h * 0.22).round().min(9.0);
        scene.quad(Quad::new(Rect::new(rect.x + 3.0, rect.y + inset, 3.0, rect.h - 2.0 * inset), color).radius(1.5));
    }
}

/// The sidebar's show/hide button: a window with its side panel marked off,
/// centered in `button`.
pub(super) fn draw_sidebar_icon(scene: &mut Scene, button: Rect, color: Color) {
    let (w, h) = (15.0, 12.0);
    let frame = Rect::new((button.x + (button.w - w) / 2.0).round(), (button.y + (button.h - h) / 2.0).round(), w, h);
    scene.quad(Quad::new(frame, Color::TRANSPARENT).radius(3.0).border(1.25, color));
    scene.fill(Rect::new(frame.x + 5.0, frame.y + 1.0, 1.25, h - 2.0), color);
}

/// A tab's number in a circle `size` across, ringed and colored by what its
/// agent is doing: a faint ring at rest, a turning dot while it works, amber
/// while it waits on you, green when it finished a turn you haven't seen.
/// Past 9 there is no number, only the circle. `t` drives the motion.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_tab_number(
    scene: &mut Scene,
    text: &mut TextSystem,
    theme: &Theme,
    index: usize,
    (cx, cy): (f32, f32),
    size: f32,
    agent: Option<AgentState>,
    done_for: f32,
    t: f32,
) {
    let r = size / 2.0;
    let circle = Rect::new(cx - r, cy - r, size, size);
    let clear = Color::hex(0x000000).alpha(0.0);
    let ink = Color::hex(0x1b1d22);
    let pulse = 0.5 + 0.5 * (t * std::f32::consts::TAU / 1.4).sin();
    let color = match agent {
        None => theme.text_faint,
        Some(AgentState::Idle) => {
            scene.quad(Quad::new(circle, clear).radius(r).border(1.0, theme.text_faint.alpha(0.7)));
            theme.text_dim
        }
        Some(AgentState::Running) => {
            scene.quad(Quad::new(circle, theme.accent.alpha(0.12)).radius(r).border(1.5, theme.accent.alpha(0.3)));
            let angle = t * std::f32::consts::TAU / 1.1;
            let (dx, dy) = ((r - 0.75) * angle.cos(), (r - 0.75) * angle.sin());
            scene.quad(Quad::new(Rect::new(cx + dx - 2.5, cy + dy - 2.5, 5.0, 5.0), theme.accent).radius(2.5));
            theme.text
        }
        Some(AgentState::Waiting) => {
            let amber = Color::hex(AGENT_WAITING);
            let halo = circle.inset(-1.0 - 3.0 * pulse, -1.0 - 3.0 * pulse);
            scene.quad(Quad::new(halo, clear).radius(halo.w / 2.0).border(1.5, amber.alpha(0.5 * (1.0 - pulse))));
            scene.quad(Quad::new(circle, amber.alpha(0.8 + 0.2 * pulse)).radius(r));
            ink
        }
        Some(AgentState::Done) => {
            draw_done_pop(scene, circle, done_for);
            ink
        }
    };
    // Every tab gets its number; two or more digits shrink to fit the circle.
    let label = format!("{}", index + 1);
    let shrink = match label.len() {
        1 => 0.0,
        2 => 1.0,
        _ => 3.0,
    };
    let font = (size * 0.5 + 2.0 - shrink).round();
    let w = text.measure(&label, UI, font);
    let baseline = (cy + text.ui_ascent(font) * 0.36).round();
    text.draw_text(scene, &label, cx - w / 2.0, baseline, UI, font, color);
}

/// A finished turn you haven't seen: the green badge springs in, flashes
/// and sends out rings, then keeps a heartbeat (a bump and a ring every
/// couple of seconds) until you look.
pub(super) fn draw_done_pop(scene: &mut Scene, circle: Rect, age: f32) {
    let green = Color::hex(AGENT_DONE);
    let r = circle.w / 2.0;
    let (cx, cy) = (circle.x + r, circle.y + r);
    let ring = |scene: &mut Scene, grow: f32, alpha: f32, width: f32| {
        let rr = r + grow;
        let rect = Rect::new(cx - rr, cy - rr, 2.0 * rr, 2.0 * rr);
        scene.quad(Quad::new(rect, Color::TRANSPARENT).radius(rr).border(width, green.alpha(alpha)));
    };
    // Rings: a burst of three on arrival, then one a beat.
    const BEAT: f32 = 2.2;
    let rings: &[f32] = if age < BEAT { &[0.0, 0.18, 0.36] } else { &[0.0] };
    let since_beat = if age < BEAT { age } else { (age - BEAT) % BEAT };
    let strength = if age < BEAT { 1.0 } else { 0.7 };
    for &delay in rings {
        let p = (since_beat - delay) / 0.9;
        if (0.0..1.0).contains(&p) {
            let ease = 1.0 - (1.0 - p).powi(3);
            ring(scene, 1.0 + ease * r * 1.4, strength * (1.0 - p), 2.5 - 1.5 * p);
        }
    }
    // A soft glow under the badge, breathing with the beat.
    let breath = 0.5 + 0.5 * (since_beat / BEAT * std::f32::consts::TAU).cos();
    let glow = r + 3.0 + 2.0 * breath;
    let halo = Rect::new(cx - glow, cy - glow, 2.0 * glow, 2.0 * glow);
    scene.quad(Quad::new(halo, green.alpha(0.18 + 0.12 * breath)).radius(glow));
    // The badge: a springy overshoot on arrival, a small bump each beat.
    let spring = 1.0 + 0.55 * (-5.0 * age).exp() * (13.0 * age).cos();
    let bump = if age >= BEAT { 1.0 + 0.12 * (-9.0 * since_beat).exp() * (since_beat * 18.0).sin().abs() } else { 1.0 };
    let scale = spring * bump;
    let br = r * scale;
    let badge = Rect::new(cx - br, cy - br, 2.0 * br, 2.0 * br);
    // White-hot at first, settling to green.
    let flash = (-4.0 * age).exp();
    scene.quad(Quad::new(badge, green.mix(Color::hex(0xffffff), 0.7 * flash)).radius(br));
}

/// An agent state's color: the accent while it works, amber while it
/// waits on you, green when it finished a turn you haven't seen.
pub(super) fn agent_color(theme: &Theme, state: AgentState, t: f32) -> Color {
    let pulse = 0.5 + 0.5 * (t * std::f32::consts::TAU / 1.4).sin();
    match state {
        AgentState::Idle => theme.text_faint,
        AgentState::Running => theme.accent.alpha(0.55 + 0.45 * pulse),
        AgentState::Waiting => Color::hex(AGENT_WAITING).alpha(0.7 + 0.3 * pulse),
        AgentState::Done => Color::hex(AGENT_DONE),
    }
}

/// The ground of an agent's header, colored by what it's doing: plain at
/// rest, blue while it works, amber (breathing) while it waits on you,
/// green when it's done and you haven't looked.
pub(super) fn agent_ground(theme: &Theme, state: AgentState, t: f32) -> Color {
    let ground = theme.panel(SURFACE_LIFT);
    let pulse = 0.5 + 0.5 * (t * std::f32::consts::TAU / 1.4).sin();
    match state {
        AgentState::Idle => ground,
        AgentState::Running => ground.mix(theme.accent, 0.17),
        AgentState::Waiting => ground.mix(Color::hex(AGENT_WAITING), 0.16 + 0.06 * pulse),
        AgentState::Done => ground.mix(Color::hex(AGENT_DONE), 0.16),
    }
}

/// How long the running header's band of light takes to cross it.
pub(super) const SWEEP_PERIOD: f32 = 2.2;

/// While the agent works, a soft band of light drifts across its header,
/// left to right, over and over.
pub(super) fn draw_running_sweep(scene: &mut Scene, theme: &Theme, area: Rect, t: f32) {
    let band = (area.w * 0.22).clamp(70.0, 220.0);
    let soft = band * 0.35;
    // From wholly off the left edge to wholly off the right, blur and all.
    let travel = area.w + band + 6.0 * soft;
    let x = area.x - band - 3.0 * soft + (t / SWEEP_PERIOD).fract() * travel;
    // Far taller than the area, so only its sides are soft: a stripe.
    let stripe = Rect::new(x, area.y - 4.0 * soft, band, area.h + 8.0 * soft);
    scene.push_clip(area);
    scene.shadow(stripe, theme.accent.alpha(0.28), 0.0, soft);
    scene.pop_clip();
}

/// An agent's state for remotes: a JSON string, or null with no agent.
pub(super) fn agent_name(state: Option<AgentState>) -> &'static str {
    match state {
        None => "null",
        Some(AgentState::Idle) => "\"idle\"",
        Some(AgentState::Running) => "\"running\"",
        Some(AgentState::Waiting) => "\"waiting\"",
        Some(AgentState::Done) => "\"done\"",
    }
}

/// "Running 2m 14s", "Waiting for you", "Done in 3m 02s", "Idle".
pub(super) fn agent_summary(info: &AgentInfo) -> String {
    match (info.state, info.turn) {
        (AgentState::Running, Some(turn)) => format!("Running {}", ui::short_duration(turn)),
        (AgentState::Done, Some(turn)) => format!("Done in {}", ui::short_duration(turn)),
        (state, _) => state.label().to_owned(),
    }
}

/// An agent's logo, where Temino has one. One-color logos come in `ink`, so
/// they show on light and dark themes alike.
pub(super) fn agent_logo(program: &'static str, ink: Color) -> Option<Arc<Picture>> {
    let (png, mono): (&[u8], bool) = match program {
        "codex" => (include_bytes!("../../assets/agents/openai.png"), true),
        "cursor-agent" | "agent" => (include_bytes!("../../assets/agents/cursor.png"), true),
        "gemini" => (include_bytes!("../../assets/agents/gemini.png"), false),
        _ => return None,
    };
    let byte = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
    let tint = mono.then(|| [byte(ink.r), byte(ink.g), byte(ink.b)]);
    // By agent and tint: decoded once per theme.
    type Logos = HashMap<(&'static str, Option<[u8; 3]>), Option<Arc<Picture>>>;
    thread_local! {
        static LOGOS: RefCell<Logos> = RefCell::new(HashMap::new());
    }
    LOGOS.with(|logos| {
        logos
            .borrow_mut()
            .entry((program, tint))
            .or_insert_with(|| {
                let mut image = image::load_from_memory(png).ok()?.into_rgba8();
                if let Some(rgb) = tint {
                    for px in image.pixels_mut() {
                        px.0[..3].copy_from_slice(&rgb);
                    }
                }
                let (w, h) = image.dimensions();
                Picture::new(w as usize, h as usize, image.into_raw())
            })
            .clone()
    })
}

/// An agent's mark, 14 points square, its left edge at `x` and centered on
/// `cy`; returns its right edge. Claude Code's is its spark, drawn as rays
/// of dots; Codex, Cursor and Gemini show their logos; the others are
/// their initial on a badge in their color.
pub(super) fn draw_agent_icon(
    scene: &mut Scene,
    text: &mut TextSystem,
    program: &'static str,
    brand: Color,
    ink: Color,
    x: f32,
    cy: f32,
) -> f32 {
    let size = 14.0;
    if let Some(logo) = agent_logo(program, ink) {
        text.draw_picture(scene, &logo, Rect::new(x, cy - size / 2.0, size, size));
    } else if program.starts_with("claude") {
        let (cx, r) = (x + size / 2.0, size / 2.0);
        for ray in 0..8 {
            let angle = ray as f32 * std::f32::consts::FRAC_PI_4 + 0.2;
            let (sin, cos) = angle.sin_cos();
            for (along, dot) in [(1.6, 2.6), (3.6, 2.4), (5.6, 2.0)] {
                let along = along * r / 7.0;
                let (dx, dy) = (cx + cos * along, cy + sin * along);
                scene.quad(Quad::new(Rect::new(dx - dot / 2.0, dy - dot / 2.0, dot, dot), brand).radius(dot / 2.0));
            }
        }
    } else {
        let badge = Rect::new(x, cy - size / 2.0, size, size);
        scene.quad(Quad::new(badge, brand).radius(4.0));
        let initial = agent::agent_brand(program).0.chars().next().unwrap_or('?').to_uppercase().to_string();
        let w = text.measure(&initial, UI_BOLD, 10.0);
        let baseline = (cy + text.ui_ascent(10.0) * 0.36).round();
        text.draw_text(scene, &initial, x + (size - w) / 2.0, baseline, UI_BOLD, 10.0, Color::hex(0xFFFFFF));
    }
    x + size
}

/// What's left of pane place `slot` under its header, which reaches
/// `reach` below its bar.
pub(super) fn under_header(slot: Rect, reach: f32) -> Rect {
    let top = (AGENT_HEADER + reach.min(header_room(slot))).min(slot.h - 1.0).max(0.0);
    Rect::new(slot.x, slot.y + top, slot.w, slot.h - top)
}

/// How far an opened agent header may reach below its bar in pane place
/// `slot`: at most half of it, leaving the terminal a few rows.
pub(super) fn header_room(slot: Rect) -> f32 {
    (slot.h * 0.5).floor().min(slot.h - 4.0 * AGENT_HEADER).max(0.0)
}

/// How a pane's header shows that its pane is the focused one.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Lit {
    /// Another pane is.
    Off,
    /// It is, and it's the tab's only pane: there's nothing to tell it
    /// from, so it only keeps a thin line.
    Soft,
    /// It is, among several: a tinted ground over a bright line.
    Full,
}

/// A header's ground, tinted with the accent while its pane is the focused
/// one of several.
fn header_ground(theme: &Theme, ground: Color, focus: Lit) -> Color {
    if focus == Lit::Full {
        ground.mix(theme.accent, 0.14)
    } else {
        ground
    }
}

/// The edge under a header at `bottom`: a hairline; while its pane is
/// focused, a line in `lit`, 2 points among several panes and a thin, dimmer
/// one alone.
fn draw_header_edge(scene: &mut Scene, theme: &Theme, bar: Rect, bottom: f32, focus: Lit, lit: Color) {
    match focus {
        Lit::Full => scene.fill(Rect::new(bar.x, bottom - 2.0, bar.w, 2.0), lit),
        Lit::Soft => scene.fill(Rect::new(bar.x, bottom - 1.0, bar.w, 1.0), theme.divider.mix(lit, 0.55)),
        Lit::Off => scene.fill(Rect::new(bar.x, bottom - 1.0, bar.w, 1.0), theme.divider),
    }
}

/// Where a pane works, as every header leads with it: the repository's
/// (or folder's) name, biggest and brightest; the folder under it, or
/// outside a repository the folders above, faint; then the branch and how
/// many files changed. At most `room` wide, the name cut last. Returns its
/// right edge.
pub(super) fn draw_place(
    scene: &mut Scene,
    text: &mut TextSystem,
    theme: &Theme,
    place: &Place,
    x: f32,
    baseline: f32,
    room: f32,
) -> f32 {
    const NAME: f32 = 13.5;
    const SMALL: f32 = 12.0;
    let end = x + room.max(0.0);
    let mut x = x;
    // Outside a repository the folders above lead in, faint: "~/Downloads/".
    if !place.repo {
        if let Some(parent) = &place.within {
            let lead = format!("{parent}/");
            let name_w = text.measure(&place.name, UI_BOLD, NAME);
            let shown = text.elide(&lead, UI, SMALL, (end - x - name_w).min(room * 0.4).max(0.0));
            if !shown.is_empty() && shown != "…" {
                x += text.draw_text(scene, &shown, x, baseline, UI, SMALL, theme.text_faint);
            }
        }
    }
    let name = text.elide(&place.name, UI_BOLD, NAME, end - x);
    x += text.draw_text(scene, &name, x, baseline, UI_BOLD, NAME, theme.text);
    if place.repo {
        if let Some(within) = &place.within {
            let shown = text.elide(&format!("/{within}"), UI, SMALL, ((end - x) * 0.5).max(0.0));
            if shown != "…" {
                x += text.draw_text(scene, &shown, x, baseline, UI, SMALL, theme.text_dim);
            }
        }
    }
    if let Some(branch) = &place.branch {
        let mut label = format!("⎇ {branch}");
        if place.changes > 0 {
            label.push_str(&format!(" ●{}", place.changes));
        }
        if end - x > 40.0 {
            x += 10.0;
            let shown = text.elide(&label, UI, SMALL, end - x);
            x += text.draw_text(scene, &shown, x, baseline, UI, SMALL, theme.variable);
        }
    }
    x
}

/// The bar over an agent's pane. Left: the agent's mark, where it works
/// (`draw_place`), a dot in the color of what it's doing with the turn's
/// clock, then what it's doing right now, else your last prompt. Right:
/// its subagents, the prompts queued behind this turn, its session's name
/// and how long it has run, and the toggle that extends it down
/// (`header_details`), up to `room` tall, the terminal moving down to make
/// room. Returns how far it reaches below the bar.
pub(super) fn draw_agent_header(
    scene: &mut Scene,
    text: &mut TextSystem,
    theme: &Theme,
    bar: Rect,
    info: &AgentInfo,
    focus: Lit,
    hover: bool,
    room: f32,
    t: f32,
) -> f32 {
    let color = agent_color(theme, info.state, t);
    let ground = header_ground(theme, agent_ground(theme, info.state, t), focus);
    scene.fill(bar, ground);
    if info.state == AgentState::Running {
        draw_running_sweep(scene, theme, bar, t);
    }
    let reach = if info.open { draw_header_details(scene, text, theme, bar, info, ground, room, t) } else { 0.0 };
    // The edge under it all, lit while the pane is focused: in the agent's
    // color while it's busy, else the accent.
    let lit = if info.state == AgentState::Idle { theme.accent } else { color };
    draw_header_edge(scene, theme, bar, bar.bottom() + reach, focus, lit);
    let font = 12.0;
    let baseline = (bar.y + bar.h / 2.0 + text.ui_ascent(font) * 0.36).round();
    let cy = bar.y + bar.h / 2.0;
    let mut x = bar.x + 10.0;
    if let Some(program) = info.program {
        let (_, brand) = agent::agent_brand(program);
        x = draw_agent_icon(scene, text, program, Color::hex(brand), theme.text, x, cy) + 8.0;
    }
    // Where it works leads, kept to half the bar.
    x = draw_place(scene, text, theme, &info.place, x, baseline, ((bar.w - 40.0) * 0.5).max(0.0)) + 12.0;
    scene.fill(Rect::new(x, cy - 7.0, 1.0, 14.0), theme.divider);
    x += 12.0;
    scene.quad(Quad::new(Rect::new(x, cy - 4.0, 8.0, 8.0), color).radius(4.0));
    x += 14.0;
    let summary = agent_summary(info);
    let ink = match info.state {
        AgentState::Waiting => Color::hex(AGENT_WAITING),
        AgentState::Done => Color::hex(AGENT_DONE),
        _ => theme.text_dim,
    };
    text.draw_text(scene, &summary, x, baseline, UI, font, ink);
    x += text.measure(&summary, UI, font) + 12.0;
    // The toggle at the right end: a triangle pointing the way it goes.
    let toggle = if hover { theme.text } else { theme.text_faint };
    draw_triangle(scene, bar.right() - 16.0, cy, if info.open { -1.0 } else { 1.0 }, toggle);
    // The right side, laid out from the edge in: the subagents whenever
    // they fit, the rest only while the line keeps some room.
    let mut right = bar.right() - 32.0;
    let subagents = (info.subagents > 0)
        .then(|| format!("{} subagent{}", info.subagents, if info.subagents == 1 { "" } else { "s" }));
    let queued = (info.queued > 0).then(|| format!("{} queued", info.queued));
    for chip in subagents.iter().chain(&queued) {
        let w = text.measure(chip, UI, 11.0);
        if right - w - 12.0 > x {
            let pill = Rect::new(right - w - 12.0, cy - 9.0, w + 12.0, 18.0);
            scene.quad(Quad::new(pill, theme.accent.alpha(0.16)).radius(9.0));
            text.draw_text(scene, chip, pill.x + 6.0, baseline, UI, 11.0, theme.accent);
            right = pill.x - 12.0;
        }
    }
    let uptime = format!("up {}", ui::short_duration(info.uptime));
    for chip in info.name.iter().chain(std::iter::once(&uptime)) {
        let w = text.measure(chip, UI, 11.0);
        if right - w < x + 160.0 {
            break;
        }
        text.draw_text(scene, chip, right - w, baseline, UI, 11.0, theme.text_faint);
        right -= w + 14.0;
    }
    if let Some((line, ink)) = header_line(theme, info) {
        let shown = text.elide(&line, UI, font, right - x - 14.0);
        text.draw_text(scene, &shown, x, baseline, UI, font, ink);
    }
    reach
}

/// The line after the agent's state: coming back to the pane, its recap;
/// while it works, what it's doing right now; else your last prompt.
/// Opened, the extension underneath shows those, so the bar doesn't.
fn header_line(theme: &Theme, info: &AgentInfo) -> Option<(String, Color)> {
    if let Some(recap) = &info.recap {
        return Some((recap.clone(), theme.text));
    }
    if info.open {
        return None;
    }
    if let Some(activity) = info.activity.as_ref().filter(|_| info.state == AgentState::Running) {
        return Some((activity.clone(), theme.text));
    }
    info.prompt.as_ref().map(|prompt| (format!("› {prompt}"), theme.text_dim))
}

/// The extended header, under the bar and above the terminal, on the same
/// ground: the recap and your prompt whole, what each subagent was
/// sent to do, the prompts queued, and where the session runs. At most
/// `room` tall, cut short with an ellipsis. Returns its height.
pub(super) fn draw_header_details(
    scene: &mut Scene,
    text: &mut TextSystem,
    theme: &Theme,
    bar: Rect,
    info: &AgentInfo,
    ground: Color,
    room: f32,
    t: f32,
) -> f32 {
    const PAD: f32 = 12.0;
    const LINE: f32 = 18.0;
    const LABEL: f32 = 20.0;
    let width = bar.w - 2.0 * PAD;
    // Rows as drawn: an indent, the text, its font and size, its ink, and
    // the height it takes.
    let mut rows: Vec<(f32, String, crate::text::FontId, f32, Color, f32)> = Vec::new();
    // What it's about, what it's doing now, and what you asked it, first.
    if let Some(task) = &info.task {
        let line = text.elide(task, UI_BOLD, 13.0, width);
        rows.push((0.0, line, UI_BOLD, 13.0, theme.text, LABEL));
    }
    let mut wrapped = |rows: &mut Vec<_>, lead: &str, body: &str, ink: Color| {
        let indent = text.measure(lead, UI, 12.0);
        for (i, line) in wrap(text, body, UI, 12.0, width - indent).into_iter().enumerate() {
            let line = if i == 0 { format!("{lead}{line}") } else { line };
            rows.push((if i == 0 { 0.0 } else { indent }, line, UI, 12.0, ink, LINE));
        }
    };
    if let Some(activity) = info.activity.as_ref().filter(|_| info.state == AgentState::Running) {
        wrapped(&mut rows, "▸ ", activity, theme.accent);
    }
    if let Some(prompt) = &info.prompt {
        wrapped(&mut rows, "› ", prompt, theme.text);
    }
    if let Some(recap) = &info.recap {
        wrapped(&mut rows, "", recap, theme.text_dim);
    }
    for (label, items) in [("Subagents", &info.subagent_tasks), ("Queued", &info.queue)] {
        if items.is_empty() {
            continue;
        }
        rows.push((0.0, label.to_owned(), UI_BOLD, 11.0, theme.text_faint, LABEL));
        for item in items {
            let item = if item.is_empty() { "Subagent" } else { item.as_str() };
            wrapped(&mut rows, "• ", item, theme.text_dim);
        }
    }
    let mut facts = vec![agent_summary(info)];
    facts.extend(info.name.clone());
    facts.push(format!("up {}", ui::short_duration(info.uptime)));
    // The bar names the repository and branch; the whole path is here.
    if !info.place.path.is_empty() {
        facts.push(info.place.path.clone());
    }
    let facts = text.elide(&facts.join(" · "), UI, 11.0, width);
    let facts_h = LABEL + 2.0;
    // Cut short to fit, the last row kept ending in an ellipsis.
    let budget = room - 2.0 * PAD + 4.0 - facts_h;
    let mut used = 0.0;
    let mut shown = 0;
    while shown < rows.len() && used + rows[shown].5 <= budget {
        used += rows[shown].5;
        shown += 1;
    }
    if shown < rows.len() {
        if let Some(row) = shown.checked_sub(1).map(|last| &mut rows[last]) {
            row.1 = text.elide(&format!("{}…", row.1.trim_end_matches('…')), row.2, row.3, width - row.0);
        }
    }
    rows.truncate(shown);
    let height = (PAD - 4.0 + used + facts_h + PAD - 4.0).min(room).max(0.0);
    if height < LINE {
        return 0.0;
    }
    let panel = Rect::new(bar.x, bar.bottom(), bar.w, height);
    scene.fill(panel, ground);
    // Where the bar meets its extension, a hairline.
    scene.fill(Rect::new(panel.x + PAD, panel.y, panel.w - 2.0 * PAD, 1.0), theme.divider.alpha(0.6));
    if info.state == AgentState::Running {
        draw_running_sweep(scene, theme, panel, t);
    }
    scene.push_clip(panel);
    let mut y = panel.y + PAD - 4.0;
    for (indent, line, font, size, ink, h) in &rows {
        let baseline = (y + h / 2.0 + text.ui_ascent(*size) * 0.36).round();
        text.draw_text(scene, line, panel.x + PAD + indent, baseline, *font, *size, *ink);
        y += h;
    }
    let baseline = (y + facts_h / 2.0 + text.ui_ascent(11.0) * 0.36).round();
    text.draw_text(scene, &facts, panel.x + PAD, baseline, UI, 11.0, theme.text_faint);
    scene.pop_clip();
    height
}

/// `line` broken at spaces into lines at most `width` wide; a word (or a
/// run of text without spaces, as in Chinese) too wide on its own is
/// broken between characters.
pub(super) fn wrap(text: &mut TextSystem, line: &str, font: crate::text::FontId, size: f32, width: f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in line.split_whitespace() {
        let joined = if current.is_empty() { word.to_owned() } else { format!("{current} {word}") };
        if text.measure(&joined, font, size) <= width {
            current = joined;
            continue;
        }
        if !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
        for c in word.chars() {
            current.push(c);
            if text.measure(&current, font, size) > width && current.chars().count() > 1 {
                current.pop();
                lines.push(std::mem::replace(&mut current, c.to_string()));
            }
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// A small filled triangle centered at (`cx`, `cy`), pointing down for
/// `dir` 1 and up for -1.
pub(super) fn draw_triangle(scene: &mut Scene, cx: f32, cy: f32, dir: f32, color: Color) {
    for row in 0..4 {
        let half = 4.0 - row as f32;
        let y = cy - 2.0 * dir + row as f32 * dir - if dir < 0.0 { 1.0 } else { 0.0 };
        scene.fill(Rect::new(cx - half, y, half * 2.0, 1.0), color);
    }
}

/// The bar over a listed program's pane: where it runs (`draw_place`),
/// its name, and how long it has run.
#[allow(clippy::too_many_arguments)]
pub(super) fn draw_program_header(
    scene: &mut Scene,
    text: &mut TextSystem,
    theme: &Theme,
    bar: Rect,
    place: &Place,
    name: &str,
    uptime: Duration,
    focus: Lit,
) {
    scene.fill(bar, header_ground(theme, theme.panel(SURFACE_LIFT), focus));
    draw_header_edge(scene, theme, bar, bar.bottom(), focus, theme.accent);
    let font = 12.0;
    let baseline = (bar.y + bar.h / 2.0 + text.ui_ascent(font) * 0.36).round();
    let cy = bar.y + bar.h / 2.0;
    let mut x = bar.x + 12.0;
    scene.quad(Quad::new(Rect::new(x, cy - 4.0, 8.0, 8.0), theme.accent).radius(4.0));
    x += 16.0;
    let uptime = format!("up {}", ui::short_duration(uptime));
    let w = text.measure(&uptime, UI, 11.0);
    let right = bar.right() - 12.0 - w - 14.0;
    x = draw_place(scene, text, theme, place, x, baseline, ((right - x) * 0.6).max(0.0)) + 14.0;
    let shown = text.elide(&format!("▸ {name}"), UI, font, (right - x).max(0.0));
    text.draw_text(scene, &shown, x, baseline, UI, font, theme.text);
    text.draw_text(scene, &uptime, bar.right() - 12.0 - w, baseline, UI, 11.0, theme.text_faint);
}

/// What the bar over a plain terminal says.
pub(super) struct ShellHeader {
    pub(super) busy: bool,
    pub(super) failed: bool,
    /// Where it is.
    pub(super) place: Place,
    /// The command running, while one is.
    pub(super) running: Option<String>,
    /// At rest: the last command, how it ended and when, and the branch.
    pub(super) detail: String,
}

/// The bar over a plain terminal: a dot (lit while a command runs, red
/// after one failed), where it is (`draw_place`), then the command running,
/// or the last command, how it ended and when.
pub(super) fn draw_shell_header(
    scene: &mut Scene,
    text: &mut TextSystem,
    theme: &Theme,
    bar: Rect,
    shell: &ShellHeader,
    focus: Lit,
) {
    let ShellHeader { busy, failed, .. } = *shell;
    scene.fill(bar, header_ground(theme, theme.panel(SURFACE_LIFT), focus));
    draw_header_edge(scene, theme, bar, bar.bottom(), focus, theme.accent);
    let font = 12.0;
    let baseline = (bar.y + bar.h / 2.0 + text.ui_ascent(font) * 0.36).round();
    let cy = bar.y + bar.h / 2.0;
    let mut x = bar.x + 12.0;
    let dot = if busy {
        theme.accent
    } else if failed {
        theme.error.alpha(0.85)
    } else {
        theme.text_faint
    };
    scene.quad(Quad::new(Rect::new(x, cy - 4.0, 8.0, 8.0), dot).radius(4.0));
    x += 16.0;
    let right = bar.right() - 12.0;
    // Where it is keeps at most half the bar, so what's going on still shows.
    x = draw_place(scene, text, theme, &shell.place, x, baseline, ((right - x) / 2.0).max(0.0)) + 14.0;
    let running = shell.running.as_ref().filter(|command| !command.is_empty()).map(|command| format!("▸ {command}"));
    let rest = [(running, theme.text), (Some(shell.detail.clone()).filter(|d| !d.is_empty()), theme.text_dim)];
    for (part, ink) in rest.into_iter().filter_map(|(part, ink)| part.map(|part| (part, ink))) {
        if right - x < 40.0 {
            break;
        }
        let shown = text.elide(&part, UI, font, right - x);
        text.draw_text(scene, &shown, x, baseline, UI, font, ink);
        x += text.measure(&shown, UI, font) + 14.0;
    }
}
