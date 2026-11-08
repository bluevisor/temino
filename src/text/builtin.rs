/// Stand-ins for symbols that terminal programs use but Haiku's fonts lack,
/// such as the tool-call markers of Claude Code-style agents (⏺ ⎿).
pub(super) fn substitute(c: char) -> Option<char> {
    Some(match c {
        '⏺' | '⬤' | '🔴' | '🟢' | '🟡' => '●',
        '⎿' => '└',
        '⏵' | '⯈' => '▶',
        '⏴' | '⯇' => '◀',
        '⏸' => '‖',
        '⏹' | '⬛' => '■',
        '⏎' => '↵',
        '✻' | '✽' | '✶' | '✳' | '✢' => '*',
        '✔' | '✅' => '✓',
        '✘' | '❌' => '✗',
        '⚠' => '!',
        _ => return None,
    })
}

/// Line weights per arm (left, right, up, down): 0 none, 1 light, 2 heavy,
/// 3 double. Indexed by `code - 0x2500` for U+2500..U+257F.
pub(super) const BOX: [[u8; 4]; 128] = {
    const fn b(l: u8, r: u8, u: u8, d: u8) -> [u8; 4] {
        [l, r, u, d]
    }
    [
        b(1, 1, 0, 0),
        b(2, 2, 0, 0),
        b(0, 0, 1, 1),
        b(0, 0, 2, 2), // ─ ━ │ ┃
        b(1, 1, 0, 0),
        b(2, 2, 0, 0),
        b(0, 0, 1, 1),
        b(0, 0, 2, 2), // dashed
        b(1, 1, 0, 0),
        b(2, 2, 0, 0),
        b(0, 0, 1, 1),
        b(0, 0, 2, 2),
        b(0, 1, 0, 1),
        b(0, 2, 0, 1),
        b(0, 1, 0, 2),
        b(0, 2, 0, 2), // ┌ ┍ ┎ ┏
        b(1, 0, 0, 1),
        b(2, 0, 0, 1),
        b(1, 0, 0, 2),
        b(2, 0, 0, 2), // ┐ ┑ ┒ ┓
        b(0, 1, 1, 0),
        b(0, 2, 1, 0),
        b(0, 1, 2, 0),
        b(0, 2, 2, 0), // └ ┕ ┖ ┗
        b(1, 0, 1, 0),
        b(2, 0, 1, 0),
        b(1, 0, 2, 0),
        b(2, 0, 2, 0), // ┘ ┙ ┚ ┛
        b(0, 1, 1, 1),
        b(0, 2, 1, 1),
        b(0, 1, 2, 1),
        b(0, 1, 1, 2), // ├ ┝ ┞ ┟
        b(0, 1, 2, 2),
        b(0, 2, 2, 1),
        b(0, 2, 1, 2),
        b(0, 2, 2, 2), // ┠ ┡ ┢ ┣
        b(1, 0, 1, 1),
        b(2, 0, 1, 1),
        b(1, 0, 2, 1),
        b(1, 0, 1, 2), // ┤ ┥ ┦ ┧
        b(1, 0, 2, 2),
        b(2, 0, 2, 1),
        b(2, 0, 1, 2),
        b(2, 0, 2, 2), // ┨ ┩ ┪ ┫
        b(1, 1, 0, 1),
        b(2, 1, 0, 1),
        b(1, 2, 0, 1),
        b(2, 2, 0, 1), // ┬ ┭ ┮ ┯
        b(1, 1, 0, 2),
        b(2, 1, 0, 2),
        b(1, 2, 0, 2),
        b(2, 2, 0, 2), // ┰ ┱ ┲ ┳
        b(1, 1, 1, 0),
        b(2, 1, 1, 0),
        b(1, 2, 1, 0),
        b(2, 2, 1, 0), // ┴ ┵ ┶ ┷
        b(1, 1, 2, 0),
        b(2, 1, 2, 0),
        b(1, 2, 2, 0),
        b(2, 2, 2, 0), // ┸ ┹ ┺ ┻
        b(1, 1, 1, 1),
        b(2, 1, 1, 1),
        b(1, 2, 1, 1),
        b(2, 2, 1, 1), // ┼ ┽ ┾ ┿
        b(1, 1, 2, 1),
        b(1, 1, 1, 2),
        b(1, 1, 2, 2),
        b(2, 1, 2, 1), // ╀ ╁ ╂ ╃
        b(1, 2, 2, 1),
        b(2, 1, 1, 2),
        b(1, 2, 1, 2),
        b(2, 2, 2, 1), // ╄ ╅ ╆ ╇
        b(2, 2, 1, 2),
        b(2, 1, 2, 2),
        b(1, 2, 2, 2),
        b(2, 2, 2, 2), // ╈ ╉ ╊ ╋
        b(1, 1, 0, 0),
        b(2, 2, 0, 0),
        b(0, 0, 1, 1),
        b(0, 0, 2, 2), // ╌ ╍ ╎ ╏
        b(3, 3, 0, 0),
        b(0, 0, 3, 3),
        b(0, 3, 0, 1),
        b(0, 1, 0, 3), // ═ ║ ╒ ╓
        b(0, 3, 0, 3),
        b(3, 0, 0, 1),
        b(1, 0, 0, 3),
        b(3, 0, 0, 3), // ╔ ╕ ╖ ╗
        b(0, 3, 1, 0),
        b(0, 1, 3, 0),
        b(0, 3, 3, 0),
        b(3, 0, 1, 0), // ╘ ╙ ╚ ╛
        b(1, 0, 3, 0),
        b(3, 0, 3, 0),
        b(0, 3, 1, 1),
        b(0, 1, 3, 3), // ╜ ╝ ╞ ╟
        b(0, 3, 3, 3),
        b(3, 0, 1, 1),
        b(1, 0, 3, 3),
        b(3, 0, 3, 3), // ╠ ╡ ╢ ╣
        b(3, 3, 0, 1),
        b(1, 1, 0, 3),
        b(3, 3, 0, 3),
        b(3, 3, 1, 0), // ╤ ╥ ╦ ╧
        b(1, 1, 3, 0),
        b(3, 3, 3, 0),
        b(3, 3, 1, 1),
        b(1, 1, 3, 3), // ╨ ╩ ╪ ╫
        b(3, 3, 3, 3),
        b(0, 1, 0, 1),
        b(1, 0, 0, 1),
        b(1, 0, 1, 0), // ╬ ╭ ╮ ╯
        b(0, 1, 1, 0),
        b(0, 0, 0, 0),
        b(0, 0, 0, 0),
        b(0, 0, 0, 0), // ╰ ╱ ╲ ╳
        b(1, 0, 0, 0),
        b(0, 0, 1, 0),
        b(0, 1, 0, 0),
        b(0, 0, 0, 1), // ╴ ╵ ╶ ╷
        b(2, 0, 0, 0),
        b(0, 0, 2, 0),
        b(0, 2, 0, 0),
        b(0, 0, 0, 2), // ╸ ╹ ╺ ╻
        b(1, 2, 0, 0),
        b(0, 0, 1, 2),
        b(2, 1, 0, 0),
        b(0, 0, 2, 1), // ╼ ╽ ╾ ╿
    ]
};

pub(super) struct Canvas {
    pub(super) out: Vec<u8>,
    pub(super) w: usize,
    pub(super) h: usize,
}

impl Canvas {
    pub(super) fn fill(&mut self, x0: usize, y0: usize, x1: usize, y1: usize, value: u8) {
        for y in y0.min(self.h)..y1.min(self.h) {
            for x in x0.min(self.w)..x1.min(self.w) {
                let pixel = &mut self.out[y * self.w + x];
                *pixel = (*pixel).max(value);
            }
        }
    }
}

pub(super) fn draw_builtin(code: u32, w: usize, h: usize, stroke: usize) -> Option<Vec<u8>> {
    let mut canvas = Canvas { out: vec![0u8; w * h], w, h };
    let light = stroke.max(1);
    let heavy = light * 2 + 1;
    // Pixel spans (lo, hi) of the line(s) for an arm of `weight` centered
    // on `center`.
    let span = |weight: u8, center: usize| -> Vec<(usize, usize)> {
        match weight {
            1 => vec![(center - light / 2, center - light / 2 + light)],
            2 => vec![(center.saturating_sub(heavy / 2), center.saturating_sub(heavy / 2) + heavy)],
            3 => {
                let gap = light + 1;
                vec![
                    (center.saturating_sub(gap + light / 2), center.saturating_sub(gap + light / 2) + light),
                    (center + gap - light / 2, center + gap - light / 2 + light),
                ]
            }
            _ => vec![],
        }
    };
    match code {
        0x2571..=0x2573 => return None, // diagonals: the font's glyphs are fine
        0x256d..=0x2570 => {
            // Rounded corner: a quarter circle joining the two arms.
            let (sx, sy): (f32, f32) = match code {
                0x256d => (1.0, 1.0),   // ╭ arms right and down
                0x256e => (-1.0, 1.0),  // ╮ left and down
                0x256f => (-1.0, -1.0), // ╯ left and up
                _ => (1.0, -1.0),       // ╰ right and up
            };
            let (v0, v1) = span(1, w / 2)[0];
            let (h0, h1) = span(1, h / 2)[0];
            let line_x = (v0 + v1) as f32 / 2.0;
            let line_y = (h0 + h1) as f32 / 2.0;
            let radius = (w.min(h) / 2) as f32;
            let (ccx, ccy) = (line_x + sx * radius, line_y + sy * radius);
            let half = light as f32 / 2.0;
            for y in 0..h {
                for x in 0..w {
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    if (px - ccx) * sx <= 0.0 && (py - ccy) * sy <= 0.0 {
                        let distance = ((px - ccx).powi(2) + (py - ccy).powi(2)).sqrt();
                        let edge = (half + 0.5 - (distance - radius).abs()).clamp(0.0, 1.0);
                        canvas.fill(x, y, x + 1, y + 1, (edge * 255.0) as u8);
                    }
                }
            }
            if sy > 0.0 {
                canvas.fill(v0, ccy as usize, v1, h, 255);
            } else {
                canvas.fill(v0, 0, v1, ccy.ceil() as usize, 255);
            }
            if sx > 0.0 {
                canvas.fill(ccx as usize, h0, w, h1, 255);
            } else {
                canvas.fill(0, h0, ccx.ceil() as usize, h1, 255);
            }
        }
        0x2500..=0x257f => {
            let [l, r, u, d] = BOX[(code - 0x2500) as usize];
            let (cx, cy) = (w / 2, h / 2);
            let gap = (light + 1) as i32;
            // The lines of an arm: (offset from the center, thickness).
            let lines = |weight: u8| -> Vec<(i32, usize)> {
                match weight {
                    1 => vec![(0, light)],
                    2 => vec![(0, heavy)],
                    3 => vec![(-gap, light), (gap, light)],
                    _ => vec![],
                }
            };
            let line_span = |center: usize, offset: i32, thickness: usize| -> (usize, usize) {
                let lo = (center as i32 + offset - (thickness / 2) as i32).max(0) as usize;
                (lo, lo + thickness)
            };
            // The crossing arms' lines sit at ±offset with this thickness.
            let crossing = |a: u8, b: u8| -> (i32, usize) {
                (if a.max(b) == 3 { gap } else { 0 }, if a.max(b) == 2 { heavy } else { light })
            };
            // A line meets the crossing lines either at the near one (when
            // an arm continues on its side, as the inner line of a corner)
            // or runs on to the far one (the outer line), so double lines
            // nest at corners and tees instead of crossing.
            let near = |offset: i32, before: u8, after: u8| -> bool {
                match offset {
                    o if o < 0 => before > 0,
                    o if o > 0 => after > 0,
                    _ => before > 0 && after > 0,
                }
            };
            let (vertical_offset, vertical_thickness) = crossing(u, d);
            let (horizontal_offset, horizontal_thickness) = crossing(l, r);
            for (offset, thickness) in lines(l) {
                let (y0, y1) = line_span(cy, offset, thickness);
                let end = if u | d == 0 {
                    cx + 1
                } else {
                    let to = if near(offset, u, d) { -vertical_offset } else { vertical_offset };
                    line_span(cx, to, vertical_thickness).1
                };
                canvas.fill(0, y0, end, y1, 255);
            }
            for (offset, thickness) in lines(r) {
                let (y0, y1) = line_span(cy, offset, thickness);
                let start = if u | d == 0 {
                    cx
                } else {
                    let from = if near(offset, u, d) { vertical_offset } else { -vertical_offset };
                    line_span(cx, from, vertical_thickness).0
                };
                canvas.fill(start, y0, w, y1, 255);
            }
            for (offset, thickness) in lines(u) {
                let (x0, x1) = line_span(cx, offset, thickness);
                let end = if l | r == 0 {
                    cy + 1
                } else {
                    let to = if near(offset, l, r) { -horizontal_offset } else { horizontal_offset };
                    line_span(cy, to, horizontal_thickness).1
                };
                canvas.fill(x0, 0, x1, end, 255);
            }
            for (offset, thickness) in lines(d) {
                let (x0, x1) = line_span(cx, offset, thickness);
                let start = if l | r == 0 {
                    cy
                } else {
                    let from = if near(offset, l, r) { horizontal_offset } else { -horizontal_offset };
                    line_span(cy, from, horizontal_thickness).0
                };
                canvas.fill(x0, start, x1, h, 255);
            }
        }
        0x2580 => canvas.fill(0, 0, w, h / 2, 255),
        0x2581..=0x2588 => {
            let eighths = (code - 0x2580) as usize;
            canvas.fill(0, h - (h * eighths + 4) / 8, w, h, 255);
        }
        0x2589..=0x258f => {
            let eighths = (0x2590 - code) as usize;
            canvas.fill(0, 0, (w * eighths + 4) / 8, h, 255);
        }
        0x2590 => canvas.fill(w / 2, 0, w, h, 255),
        0x2591 => canvas.fill(0, 0, w, h, 64),
        0x2592 => canvas.fill(0, 0, w, h, 128),
        0x2593 => canvas.fill(0, 0, w, h, 192),
        0x2594 => canvas.fill(0, 0, w, (h + 4) / 8, 255),
        0x2595 => canvas.fill(w - (w + 4) / 8, 0, w, h, 255),
        0x2596..=0x259f => {
            // Quadrant bits: upper-left, upper-right, lower-left, lower-right.
            let quadrants: u8 = match code {
                0x2596 => 0b0100,
                0x2597 => 0b1000,
                0x2598 => 0b0001,
                0x2599 => 0b1101,
                0x259a => 0b1001,
                0x259b => 0b0111,
                0x259c => 0b1011,
                0x259d => 0b0010,
                0x259e => 0b0110,
                _ => 0b1110,
            };
            let (mx, my) = (w / 2, h / 2);
            for (bit, (x0, y0, x1, y1)) in
                [(0, 0, mx, my), (mx, 0, w, my), (0, my, mx, h), (mx, my, w, h)].into_iter().enumerate()
            {
                if quadrants & (1 << bit) != 0 {
                    canvas.fill(x0, y0, x1, y1, 255);
                }
            }
        }
        _ => return None,
    }
    Some(canvas.out)
}
