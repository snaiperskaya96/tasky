//! A generated application icon, so the binary needs no image assets.
//!
//! A dark rounded badge with a few rising "signal" bars: legible down to the
//! 16px a tray icon is usually scaled to, and crisp at 64px for the title bar.

pub const SIZE: u32 = 64;

pub struct Icon {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub fn app_icon() -> Icon {
    render(SIZE)
}

struct Bar {
    x0: f32,
    x1: f32,
    y0: f32,
    y1: f32,
}

fn render(size: u32) -> Icon {
    let mut icon = Icon {
        width: size,
        height: size,
        rgba: vec![0u8; (size * size * 4) as usize],
    };
    let s = size as f32;

    const BG: [u8; 3] = [0x11, 0x16, 0x27];
    const DIM: [u8; 3] = [0x1e, 0x5a, 0x7a];
    const ACCENT: [u8; 3] = [0x38, 0xbd, 0xf8];

    let inset = s * 0.08;
    let radius = s * 0.24;

    // Five bars rising from the baseline, tallest in the middle.
    let heights = [0.30, 0.50, 0.74, 0.58, 0.40];
    let colours = [DIM, ACCENT, ACCENT, ACCENT, DIM];

    let count = heights.len() as f32;
    let gap = s * 0.03;
    let bar_w = (s - inset * 2.0 - gap * (count - 1.0)) / count;
    let baseline = s - inset;

    let bars: Vec<Bar> = heights
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let x0 = inset + i as f32 * (bar_w + gap);
            Bar {
                x0,
                x1: x0 + bar_w,
                y0: (baseline - s * h * 0.82).max(inset),
                y1: baseline,
            }
        })
        .collect();

    let bar_at = |x: f32, y: f32| -> Option<usize> {
        bars.iter()
            .position(|b| x >= b.x0 && x <= b.x1 && y >= b.y0 && y <= b.y1)
    };

    for py in 0..size {
        for px in 0..size {
            // 4x4 supersampling is plenty for icons this small.
            let mut badge = 0.0_f32;
            let mut bar = 0.0_f32;
            let mut hit: Option<usize> = None;

            for sy in 0..4 {
                for sx in 0..4 {
                    let x = px as f32 + (sx as f32 + 0.5) / 4.0;
                    let y = py as f32 + (sy as f32 + 0.5) / 4.0;
                    if !rounded_rect(x, y, inset, inset, s - inset, s - inset, radius) {
                        continue;
                    }
                    if let Some(i) = bar_at(x, y) {
                        hit = Some(i);
                        bar += 1.0;
                    }
                    badge += 1.0;
                }
            }

            if badge == 0.0 {
                continue;
            }

            let (colour, alpha) = match hit {
                Some(i) => (colours[i], bar / 16.0),
                None => (BG, (badge / 16.0) * 0.99),
            };

            let i = ((py * size + px) * 4) as usize;
            icon.rgba[i] = colour[0];
            icon.rgba[i + 1] = colour[1];
            icon.rgba[i + 2] = colour[2];
            icon.rgba[i + 3] = (alpha * 255.0).round() as u8;
        }
    }

    icon
}

/// Squared-distance test against a rounded rectangle.
fn rounded_rect(x: f32, y: f32, x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> bool {
    if x < x0 || x > x1 || y < y0 || y > y1 {
        return false;
    }
    // The core (excluding the four rounded corners) covers everything else.
    let core_x0 = x0 + r;
    let core_x1 = (x1 - r).max(core_x0);
    let core_y0 = y0 + r;
    let core_y1 = (y1 - r).max(core_y0);
    if x >= core_x0 && x <= core_x1 {
        return true;
    }
    if y >= core_y0 && y <= core_y1 {
        return true;
    }
    let cx = if x < core_x0 { core_x0 } else { core_x1 };
    let cy = if y < core_y0 { core_y0 } else { core_y1 };
    let dx = x - cx;
    let dy = y - cy;
    dx * dx + dy * dy <= r * r
}
