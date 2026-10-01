//! Client-side window decorations for platforms that draw none (GNOME on Wayland): a resize frame
//! around the window and the window-control buttons. The frame and edge handling are adapted from
//! GPUI's `window_shadow` example (Apache-2.0).

use gpui::{
    App, Bounds, CursorStyle, Decorations, HitboxBehavior, Hsla, MouseButton, Pixels, Point, ResizeEdge, Size, Tiling,
    Window, canvas, div, point, prelude::*, px, rgb,
};

use crate::theme::{LINE, MUTED, RAISED, TEXT};

/// Width of the invisible resize border (and shadow) around an untiled window.
pub const INSET: Pixels = px(8.0);

/// Which edge or corner a window position is on, ignoring tiled edges.
pub fn resize_edge(pos: Point<Pixels>, size: Size<Pixels>, tiling: Tiling) -> Option<ResizeEdge> {
    let top = pos.y < INSET && !tiling.top;
    let bottom = pos.y > size.height - INSET && !tiling.bottom;
    let left = pos.x < INSET && !tiling.left;
    let right = pos.x > size.width - INSET && !tiling.right;
    Some(match (top, bottom, left, right) {
        (true, _, true, _) => ResizeEdge::TopLeft,
        (true, _, _, true) => ResizeEdge::TopRight,
        (_, true, true, _) => ResizeEdge::BottomLeft,
        (_, true, _, true) => ResizeEdge::BottomRight,
        (true, ..) => ResizeEdge::Top,
        (_, true, ..) => ResizeEdge::Bottom,
        (_, _, true, _) => ResizeEdge::Left,
        (_, _, _, true) => ResizeEdge::Right,
        _ => return None,
    })
}

fn cursor_for(edge: ResizeEdge) -> CursorStyle {
    match edge {
        ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
        ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
        ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
        ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
    }
}

/// Wrap the window content in a resize frame when the window uses client-side decorations.
pub fn frame(window: &mut Window, content: impl IntoElement) -> impl IntoElement {
    let decorations = window.window_decorations();
    if let Decorations::Client { .. } = decorations {
        window.set_client_inset(INSET);
    }
    div()
        .id("window-frame")
        .size_full()
        .map(|frame| match decorations {
            Decorations::Server => frame,
            Decorations::Client { tiling } => frame
                .child(
                    canvas(
                        |_bounds, window, _cx| {
                            window.insert_hitbox(
                                Bounds::new(point(px(0.0), px(0.0)), window.window_bounds().get_bounds().size),
                                HitboxBehavior::Normal,
                            )
                        },
                        move |_bounds, hitbox, window, _cx| {
                            let size = window.window_bounds().get_bounds().size;
                            if let Some(edge) = resize_edge(window.mouse_position(), size, tiling) {
                                window.set_cursor_style(cursor_for(edge), &hitbox);
                            }
                        },
                    )
                    .size_full()
                    .absolute(),
                )
                .when(!tiling.top, |frame| frame.pt(INSET))
                .when(!tiling.bottom, |frame| frame.pb(INSET))
                .when(!tiling.left, |frame| frame.pl(INSET))
                .when(!tiling.right, |frame| frame.pr(INSET))
                .on_mouse_move(|_, window, _| window.refresh())
                .on_mouse_down(MouseButton::Left, move |event, window, _| {
                    let size = window.window_bounds().get_bounds().size;
                    if let Some(edge) = resize_edge(event.position, size, tiling) {
                        window.start_window_resize(edge);
                    }
                }),
        })
        .child(
            div()
                .size_full()
                .cursor(CursorStyle::Arrow)
                .map(|inner| match decorations {
                    Decorations::Server => inner,
                    Decorations::Client { tiling } => inner
                        .border_color(rgb(LINE))
                        .when(!tiling.top, |inner| inner.border_t_1())
                        .when(!tiling.bottom, |inner| inner.border_b_1())
                        .when(!tiling.left, |inner| inner.border_l_1())
                        .when(!tiling.right, |inner| inner.border_r_1())
                        .when(!tiling.is_tiled(), |inner| {
                            inner.shadow(vec![gpui::BoxShadow::new(px(0.), px(0.), Hsla { h: 0., s: 0., l: 0., a: 0.4 }).blur_radius(INSET / 2.)])
                        }),
                })
                .on_mouse_move(|_, _, cx| cx.stop_propagation())
                .child(content),
        )
}

/// Minimize / maximize / close buttons, as the platform supports them.
pub fn window_controls(window: &Window, on_close: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static) -> impl IntoElement {
    let controls = window.window_controls();
    let button = |id: &'static str, glyph: &'static str| {
        div()
            .id(id)
            .size(px(30.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(rgb(RAISED))
            .text_sm()
            .text_color(rgb(MUTED))
            .hover(|style| style.text_color(rgb(TEXT)).bg(rgb(LINE)))
            .child(glyph)
    };
    div()
        .flex_none()
        .h_full()
        .pr_3()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .when(controls.minimize, |row| row.child(button("minimize", "–").on_click(|_, window, _| window.minimize_window())))
        .when(controls.maximize, |row| row.child(button("maximize", "□").on_click(|_, window, _| window.zoom_window())))
        .child(button("close", "×").on_click(on_close))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Tiling, point, px, size};

    const WINDOW: (f32, f32) = (800.0, 600.0);

    fn edge_at(x: f32, y: f32, tiling: Tiling) -> Option<ResizeEdge> {
        resize_edge(point(px(x), px(y)), size(px(WINDOW.0), px(WINDOW.1)), tiling)
    }

    #[test]
    fn edges_and_corners_inside_the_inset_resize() {
        let free = Tiling::default();
        assert_eq!(edge_at(2.0, 2.0, free), Some(ResizeEdge::TopLeft));
        assert_eq!(edge_at(400.0, 2.0, free), Some(ResizeEdge::Top));
        assert_eq!(edge_at(798.0, 300.0, free), Some(ResizeEdge::Right));
        assert_eq!(edge_at(798.0, 598.0, free), Some(ResizeEdge::BottomRight));
        assert_eq!(edge_at(2.0, 598.0, free), Some(ResizeEdge::BottomLeft));
        assert_eq!(edge_at(400.0, 300.0, free), None);
    }

    #[test]
    fn tiled_edges_do_not_resize() {
        let left_tiled = Tiling { left: true, top: true, bottom: true, right: false };
        assert_eq!(edge_at(2.0, 300.0, left_tiled), None);
        assert_eq!(edge_at(2.0, 2.0, left_tiled), None);
        assert_eq!(edge_at(798.0, 300.0, left_tiled), Some(ResizeEdge::Right));
    }
}
