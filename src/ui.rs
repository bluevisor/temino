//! Drawing helpers: terminal grids, command syntax coloring, chips.

use std::time::Duration;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::Column;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::CursorShape;

use crate::complete::Commands;
use crate::scene::{Quad, Rect, Scene};
use crate::session::Block;
use crate::text::{Style, TextSystem, UI};
use crate::theme::{Color, Theme};

/// A normalized selection: start <= end, as (line, column) in a block.
pub type Span = ((usize, usize), (usize, usize));

fn cell_colors(cell: &Cell, theme: &Theme, colors: &Colors) -> (Color, Option<Color>) {
    let mut fg = theme.resolve(cell.fg, colors, true).unwrap_or(theme.text);
    let mut bg = theme.resolve(cell.bg, colors, false);
    if cell.flags.contains(Flags::DIM) {
        fg = fg.mix(theme.background, 0.4);
    }
    if cell.flags.contains(Flags::INVERSE) {
        let inverted = fg;
        fg = bg.unwrap_or(theme.background);
        bg = Some(inverted);
    }
    (fg, bg)
}

/// Output line `index` of `block` for remote clients: a JSON array of
/// `[text, fg, bg, flags]` runs (flags: 1 bold, 2 italic, 4 underline,
/// 8 strikeout, 16 two cells wide), colors resolved through the theme like
/// the GPU view's.
pub fn remote_row(block: &Block, index: usize, theme: &Theme) -> String {
    let term = &block.term;
    let grid = term.grid();
    let colors = term.colors();
    let row = &grid[block.grid_line(index)];
    let columns = grid.columns();
    // Trailing blanks without a background aren't worth sending.
    let mut end = columns;
    while end > 0 {
        let c = &row[Column(end - 1)];
        if (c.c != ' ' && c.c != '\t') || cell_colors(c, theme, colors).1.is_some() {
            break;
        }
        end -= 1;
    }
    let mut runs: Vec<(String, String, Option<String>, u8)> = Vec::new();
    for column in 0..end {
        let c = &row[Column(column)];
        if c.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        let (fg, bg) = cell_colors(c, theme, colors);
        let mut flags = 0u8;
        if c.flags.contains(Flags::BOLD) {
            flags |= 1;
        }
        if c.flags.contains(Flags::ITALIC) {
            flags |= 2;
        }
        if c.flags.intersects(Flags::ALL_UNDERLINES) {
            flags |= 4;
        }
        if c.flags.contains(Flags::STRIKEOUT) {
            flags |= 8;
        }
        if c.flags.contains(Flags::WIDE_CHAR) {
            flags |= 16;
        }
        let (fg, bg) = (fg.css(), bg.map(Color::css));
        let mut piece = String::new();
        if c.flags.contains(Flags::HIDDEN) || c.c == '\t' {
            piece.push(' ');
        } else {
            piece.push(c.c);
            piece.extend(c.zerowidth().unwrap_or(&[]));
        }
        match runs.last_mut() {
            Some(run) if run.1 == fg && run.2 == bg && run.3 == flags => run.0.push_str(&piece),
            _ => runs.push((piece, fg, bg, flags)),
        }
    }
    let runs: Vec<String> = runs
        .into_iter()
        .map(|(text, fg, bg, flags)| {
            format!(
                "[{},\"{fg}\",{},{flags}]",
                crate::remote::quote(&text),
                bg.map_or("null".to_owned(), |bg| format!("\"{bg}\""))
            )
        })
        .collect();
    format!("[{}]", runs.join(","))
}

pub struct GridView<'a> {
    pub block: &'a Block,
    pub x: f32,
    pub y: f32,
    /// Output lines [first, last) are drawn, `first` at `y`.
    pub first: usize,
    pub last: usize,
    pub cursor: bool,
    pub focused: bool,
    pub selection: Option<Span>,
    /// Search matches: (line, start column, end column, current).
    pub marks: Vec<(usize, usize, usize, bool)>,
    pub blink_on: bool,
}

pub fn draw_grid(scene: &mut Scene, text: &mut TextSystem, theme: &Theme, view: GridView) {
    let cell = text.cell;
    let term = &view.block.term;
    let grid = term.grid();
    let colors = term.colors();
    let columns = grid.columns();
    let history = grid.history_size();

    for index in view.first..view.last {
        let row = &grid[view.block.grid_line(index)];
        let y = view.y + (index - view.first) as f32 * cell.height;

        // Backgrounds, merged into runs.
        let mut run: Option<(usize, Color)> = None;
        for column in 0..=columns {
            let bg = if column < columns { cell_colors(&row[Column(column)], theme, colors).1 } else { None };
            match (run, bg) {
                (Some((_, a)), Some(b)) if a == b => {}
                _ => {
                    if let Some((start, color)) = run.take() {
                        let x = view.x + start as f32 * cell.width;
                        scene.fill(Rect::new(x, y, (column - start) as f32 * cell.width, cell.height), color);
                    }
                    run = bg.map(|color| (column, color));
                }
            }
        }

        if let Some(((l0, c0), (l1, c1))) = view.selection {
            if index >= l0 && index <= l1 {
                let from = if index == l0 { c0 } else { 0 };
                let to = if index == l1 { c1 + 1 } else { columns };
                if to > from {
                    let rect =
                        Rect::new(view.x + from as f32 * cell.width, y, (to - from) as f32 * cell.width, cell.height);
                    scene.fill(rect, theme.selection);
                }
            }
        }

        for &(_, from, to, current) in view.marks.iter().filter(|m| m.0 == index) {
            let rect = Rect::new(view.x + from as f32 * cell.width, y, (to - from) as f32 * cell.width, cell.height);
            if current {
                scene.quad(Quad::new(rect, theme.warning.alpha(0.55)).radius(2.0).border(1.0, theme.warning));
            } else {
                scene.quad(Quad::new(rect, theme.warning.alpha(0.22)).radius(2.0));
            }
        }

        // Ligatures, shaped a run of one style at a time.
        let mut ligature: Vec<Option<u16>> = vec![None; columns];
        let mut start = 0;
        while start < columns {
            let style_of =
                |c: &Cell| Style { bold: c.flags.contains(Flags::BOLD), italic: c.flags.contains(Flags::ITALIC) };
            let style = style_of(&row[Column(start)]);
            let mut end = start;
            while end < columns && row[Column(end)].c.is_ascii_graphic() && style_of(&row[Column(end)]) == style {
                end += 1;
            }
            if end > start + 1 {
                let run: Vec<char> = (start..end).map(|i| row[Column(i)].c).collect();
                for (i, glyph) in text.ligatures(&run, style).into_iter().enumerate() {
                    ligature[start + i] = glyph;
                }
            }
            start = end.max(start + 1);
        }

        for column in 0..columns {
            let c = &row[Column(column)];
            if c.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            let (fg, _) = cell_colors(c, theme, colors);
            let x = view.x + column as f32 * cell.width;
            let span = if c.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
            if !c.flags.contains(Flags::HIDDEN) {
                let style = Style { bold: c.flags.contains(Flags::BOLD), italic: c.flags.contains(Flags::ITALIC) };
                match ligature[column] {
                    Some(glyph) => text.draw_mono_glyph(scene, glyph, x, y, style, fg),
                    None => text.draw_cluster(scene, c.c, c.zerowidth().unwrap_or(&[]), x, y, style, fg, span),
                }
            }
            let width = cell.width * span as f32;
            if c.flags.intersects(Flags::ALL_UNDERLINES) {
                scene.fill(Rect::new(x, y + cell.underline, width, cell.stroke), fg);
                if c.flags.contains(Flags::DOUBLE_UNDERLINE) {
                    scene.fill(Rect::new(x, y + cell.underline + 2.0 * cell.stroke, width, cell.stroke), fg);
                }
            }
            if c.flags.contains(Flags::STRIKEOUT) {
                scene.fill(Rect::new(x, y + (cell.baseline - cell.size * 0.3).round(), width, cell.stroke), fg);
            }
        }
    }

    if view.cursor && term.mode().contains(TermMode::SHOW_CURSOR) {
        let point = grid.cursor.point;
        let index = (history as i32 + point.line.0) as usize;
        if view.focused && index >= view.first && index < view.last {
            // Marked whether or not the cursor is in its blink-off phase.
            let x = view.x + point.column.0 as f32 * cell.width;
            let y = view.y + (index - view.first) as f32 * cell.height;
            scene.mark_caret(Rect::new(x, y, cell.width, cell.height));
        }
        if index >= view.first && index < view.last && (view.blink_on || !view.focused) {
            let x = view.x + point.column.0 as f32 * cell.width;
            let y = view.y + (index - view.first) as f32 * cell.height;
            let rect = Rect::new(x, y, cell.width, cell.height);
            let shape = term.cursor_style().shape;
            if !view.focused {
                scene.quad(Quad::new(rect, Color::TRANSPARENT).border(1.0, theme.cursor));
            } else {
                match shape {
                    CursorShape::Beam => scene.fill(Rect::new(x, y, 2.0, cell.height), theme.cursor),
                    CursorShape::Underline => {
                        scene.fill(Rect::new(x, y + cell.height - 2.0, cell.width, 2.0), theme.cursor)
                    }
                    CursorShape::HollowBlock => {
                        scene.quad(Quad::new(rect, Color::TRANSPARENT).border(1.0, theme.cursor))
                    }
                    CursorShape::Hidden => {}
                    CursorShape::Block => {
                        scene.fill(rect, theme.cursor);
                        let under = &grid[point.line][point.column];
                        if under.c != ' ' {
                            let style = Style {
                                bold: under.flags.contains(Flags::BOLD),
                                italic: under.flags.contains(Flags::ITALIC),
                            };
                            text.draw_cell(scene, under.c, x, y, style, theme.background, 1);
                        }
                    }
                }
            }
        }
    }
}

/// Colors for the editor's command text: commands, flags, strings,
/// variables, operators. Returns (byte range start, end, color).
pub fn highlight(source: &str, cursor: usize, commands: &mut Commands, theme: &Theme) -> Vec<(usize, usize, Color)> {
    let mut spans = Vec::new();
    let bytes = source.as_bytes();
    let mut i = 0;
    let mut command_position = true;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b' ' || b == b'\t' {
            i += 1;
            continue;
        }
        if b == b'\n' || b == b';' || b == b'|' || b == b'&' || b == b'(' || b == b')' || b == b'<' || b == b'>' {
            let start = i;
            while i < bytes.len() && matches!(bytes[i], b';' | b'|' | b'&' | b'(' | b')' | b'<' | b'>') {
                i += 1;
            }
            if b == b'\n' {
                i += 1;
            } else {
                spans.push((start, i, theme.accent));
            }
            command_position = !matches!(b, b'<' | b'>' | b')');
            continue;
        }
        if b == b'#' && (i == 0 || bytes[i - 1] == b' ') {
            let end = source[i..].find('\n').map_or(bytes.len(), |n| i + n);
            spans.push((i, end, theme.text_faint));
            i = end;
            continue;
        }
        // A word, with quotes and escapes kept inside it.
        let start = i;
        let mut parts = Vec::new();
        while i < bytes.len()
            && !matches!(bytes[i], b' ' | b'\t' | b'\n' | b';' | b'|' | b'&' | b'(' | b')' | b'<' | b'>')
        {
            match bytes[i] {
                b'\'' | b'"' => {
                    let quote = bytes[i];
                    let q = i;
                    i += 1;
                    while i < bytes.len() && bytes[i] != quote {
                        i += if bytes[i] == b'\\' && quote == b'"' { 2 } else { 1 };
                    }
                    i = (i + 1).min(bytes.len());
                    parts.push((q, i, theme.string));
                }
                b'$' => {
                    let v = i;
                    i += 1;
                    if i < bytes.len() && bytes[i] == b'{' {
                        while i < bytes.len() && bytes[i] != b'}' {
                            i += 1;
                        }
                        i = (i + 1).min(bytes.len());
                    } else {
                        while i < bytes.len()
                            && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'?')
                        {
                            i += 1;
                        }
                    }
                    parts.push((v, i, theme.variable));
                }
                b'\\' => i = (i + 2).min(bytes.len()),
                _ => i += 1,
            }
        }
        let word = &source[start..i];
        if command_position {
            let assignment = word.contains('=') && !word.starts_with('=');
            if !assignment {
                let editing = cursor == i;
                let color = if commands.exists(word) {
                    theme.success
                } else if editing {
                    theme.text
                } else {
                    theme.error
                };
                spans.push((start, i, color));
                command_position = matches!(word, "sudo" | "time" | "exec" | "nohup" | "env");
            }
        } else if word.starts_with('-') {
            spans.push((start, i, theme.ansi[6]));
        }
        spans.extend(parts);
    }
    spans
}

pub fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs_f32();
    if seconds < 1.0 {
        format!("{}ms", duration.as_millis())
    } else if seconds < 60.0 {
        format!("{seconds:.1}s")
    } else {
        let total = duration.as_secs();
        format!("{}m {:02}s", total / 60, total % 60)
    }
}

/// `~` for the home directory, and only the last components of deep paths.
pub fn short_path(path: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/boot/home".into());
    let path = match path.strip_prefix(&home) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_owned(),
    };
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() > 5 {
        format!("{}/…/{}", parts[0], parts[parts.len() - 3..].join("/"))
    } else {
        path
    }
}

/// A rounded label; returns its width.
pub fn chip(
    scene: &mut Scene,
    text: &mut TextSystem,
    label: &str,
    x: f32,
    y: f32,
    height: f32,
    fill: Color,
    color: Color,
) -> f32 {
    let size = 12.0;
    let width = text.measure(label, UI, size) + 16.0;
    scene.quad(Quad::new(Rect::new(x, y, width, height), fill).radius(6.0));
    let baseline = y + (height + text.ui_ascent(size) * 0.72) / 2.0 + 1.0;
    text.draw_text(scene, label, x + 8.0, baseline.round(), UI, size, color);
    width
}

/// A keycap followed by what it does; returns the total width.
pub fn hint(scene: &mut Scene, text: &mut TextSystem, theme: &Theme, key: &str, label: &str, x: f32, y: f32) -> f32 {
    let key_width = text.measure(key, UI, 11.0) + 12.0;
    scene.quad(Quad::new(Rect::new(x, y, key_width, 20.0), theme.raised).radius(5.0).border(1.0, theme.divider));
    text.draw_text(scene, key, x + 6.0, y + 14.0, UI, 11.0, theme.text_dim);
    let label_width = text.draw_text(scene, label, x + key_width + 7.0, y + 14.0, UI, 12.0, theme.text_faint);
    key_width + 7.0 + label_width
}

/// "45s", "3m 07s", "1h 04m".
pub fn short_duration(d: std::time::Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, s / 60 % 60),
    }
}
