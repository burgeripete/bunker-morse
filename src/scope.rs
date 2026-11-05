//! The signal displays, drawn directly because they redraw twenty times a
//! second with hundreds of bars.

use gpui_kit::{Hsla, IntoElement, Styled as _, canvas, fill, point, px, size};

/// Thirty seconds at the engine's signal rate.
pub const SCOPE_READINGS: usize = 600;
const SCOPE_HEIGHT: f32 = 110.0;
const SPECTRUM_HEIGHT: f32 = 70.0;
const KEY_STRIP_HEIGHT: f32 = 8.0;
const FULL_SCALE_DB: f32 = 60.0;
const GRID_STEP_DB: f32 = 20.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    pub snr_db: f32,
    pub threshold_db: f32,
    pub on: bool,
}

#[derive(Clone, Copy)]
pub struct Colors {
    pub background: Hsla,
    pub grid: Hsla,
    pub idle: Hsla,
    pub active: Hsla,
    pub threshold: Hsla,
}

fn height_of(db: f32, scale: f32) -> f32 {
    (db / FULL_SCALE_DB).clamp(0.0, 1.0) * scale
}

/// Tone level over time against the threshold, newest at the right. The strip
/// underneath shows what the detector keyed, so dots and dashes read directly.
pub fn scope(readings: Vec<Reading>, colors: Colors) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            window.paint_quad(fill(bounds, colors.background));
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            let (left, top) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
            let trace_height = height - KEY_STRIP_HEIGHT - 2.0;
            let baseline = top + trace_height;

            let mut grid = GRID_STEP_DB;
            while grid < FULL_SCALE_DB {
                let y = baseline - height_of(grid, trace_height);
                let line = gpui_kit::Bounds::new(point(px(left), px(y)), size(px(width), px(1.0)));
                window.paint_quad(fill(line, colors.grid));
                grid += GRID_STEP_DB;
            }

            let column = width / SCOPE_READINGS as f32;
            let first_x = left + width - readings.len() as f32 * column;
            let bar_width = column.max(1.0);
            for (index, reading) in readings.iter().enumerate() {
                let x = first_x + index as f32 * column;
                let bar = height_of(reading.snr_db, trace_height);
                let color = if reading.on {
                    colors.active
                } else {
                    colors.idle
                };
                let body = gpui_kit::Bounds::new(
                    point(px(x), px(baseline - bar)),
                    size(px(bar_width), px(bar)),
                );
                window.paint_quad(fill(body, color));

                let threshold = gpui_kit::Bounds::new(
                    point(
                        px(x),
                        px(baseline - height_of(reading.threshold_db, trace_height)),
                    ),
                    size(px(bar_width), px(1.0)),
                );
                window.paint_quad(fill(threshold, colors.threshold));

                if reading.on {
                    let key = gpui_kit::Bounds::new(
                        point(px(x), px(top + height - KEY_STRIP_HEIGHT)),
                        size(px(bar_width), px(KEY_STRIP_HEIGHT)),
                    );
                    window.paint_quad(fill(key, colors.active));
                }
            }
        },
    )
    .w_full()
    .h(px(SCOPE_HEIGHT))
}

/// The detector's frequency bank, with the locked tone's bin highlighted.
pub fn spectrum(bins: Vec<f32>, locked: Option<usize>, colors: Colors) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            window.paint_quad(fill(bounds, colors.background));
            if bins.is_empty() {
                return;
            }
            let width = f32::from(bounds.size.width);
            let height = f32::from(bounds.size.height);
            let (left, top) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y));
            let column = width / bins.len() as f32;
            for (index, db) in bins.iter().enumerate() {
                let bar = height_of(*db, height);
                let color = if locked == Some(index) {
                    colors.active
                } else {
                    colors.idle
                };
                let body = gpui_kit::Bounds::new(
                    point(px(left + index as f32 * column), px(top + height - bar)),
                    size(px((column - 1.0).max(1.0)), px(bar)),
                );
                window.paint_quad(fill(body, color));
            }
        },
    )
    .w_full()
    .h(px(SPECTRUM_HEIGHT))
}
