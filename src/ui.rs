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
                text.draw_cell(scene, c.c, x, y, style, fg, span);
                if let Some(marks) = c.zerowidth() {
                    for &mark in marks {
                        text.draw_cell(scene, mark, x, y, style, fg, span);
                    }
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
