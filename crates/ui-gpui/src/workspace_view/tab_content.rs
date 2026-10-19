use super::layout::{CARD_PAD, content_frame};
use super::{PANE_GAP, WorkspaceView};

use app_core::attention::without_claude_glyph;
use app_core::split_tree::{self, Axis, NodeId};
use app_core::theme_colors::mix;
use app_core::workspace::{DirBadge, MAX_PANES_PER_TAB};
use gpui::{
    App, Bounds, Context, CursorStyle, Div, DragMoveEvent, FontWeight, IntoElement, MouseButton, MouseDownEvent,
    Pixels, Point, Window, canvas, div, prelude::*, px, relative, rgb, rgba, svg,
};
use protocol::message::SessionStatus;

use app_core::menus::{self, MenuAction, MenuEntry, MenuItem};
use app_core::session_tree::{Tone, Unseen, display_path, leaf_tone, status_tone, tone_word};
use app_core::shortcuts::{self, Platform};

use crate::assets::{CLOSE_ICON, PROMPT_ICON, SPLIT_DOWN_ICON, SPLIT_RIGHT_ICON};
use crate::context_menu::OpenMenu;
use crate::hover_card::{self, HoverCard};
use crate::pane_drag::DraggedPane;
use crate::settings_store::SettingsStore;
use crate::theme::{
    RADIUS, RADIUS_LARGE, RADIUS_SMALL, Theme, container_icon, dir_badge, pin_icon, side_line, ssh_icon, sweeping,
    theme, working_sweep,
};

impl WorkspaceView {
    /// The split right and split down buttons in the top right corner of a pane's terminal area (so under its
    /// header): visible while the pane is hovered or focused, or while its popup is open.
    fn split_buttons(
        &self,
        leaf: NodeId,
        index: usize,
        focused: bool,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let settings = cx.global::<SettingsStore>().settings();
        let popup_open = self.pane_menu.as_ref().is_some_and(|menu| {
            matches!(
                menu.items.first(),
                Some(MenuItem::Entry(MenuEntry { action: MenuAction::SplitPane { leaf: open, .. }, .. })) if *open == leaf
            )
        });
        let group = format!("pane-card-{index}");
        let can_split = self.workspace.can_split();
        let button = |axis: Axis, label: &'static str, shortcut: &'static str, icon: &'static str| {
            let platform = Platform::current();
            let title = match shortcuts::current(shortcut, &settings.shortcuts, platform) {
                Some(keystroke) => format!("{label} ({})", shortcuts::label(Some(&keystroke), platform)),
                None => label.to_string(),
            };
            let title = if can_split { title } else { format!("{title} ({MAX_PANES_PER_TAB} panes max)") };
            let card = HoverCard { title, status: None, dir: None, container: false, host: None, folder: None };
            div()
                .id((label, index))
                .flex_none()
                .size(theme.ui(22.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(RADIUS_SMALL - 2.))
                .when(can_split, |button| button.cursor_pointer().hover(|style| style.bg(rgb(theme.tokens.raised))))
                .tooltip(hover_card::tooltip(card))
                .child(svg().path(icon).size(theme.ui(14.)).text_color(rgb(theme.tokens.muted)).opacity(if can_split {
                    1.
                } else {
                    0.35
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        if can_split {
                            this.open_pane_menu(leaf, axis, event.position + gpui::point(px(0.), px(14.)), window, cx);
                        }
                        // Keep focus on the popup: the workspace's tracked focus must not take it back.
                        window.prevent_default();
                        cx.stop_propagation();
                    }),
                )
        };
        let close_title = {
            let platform = Platform::current();
            match shortcuts::current("closePane", &settings.shortcuts, platform) {
                Some(keystroke) => format!("Close pane ({})", shortcuts::label(Some(&keystroke), platform)),
                None => "Close pane".to_string(),
            }
        };
        let close_card =
            HoverCard { title: close_title, status: None, dir: None, container: false, host: None, folder: None };
        let danger = theme.tokens.danger;
        div()
            .absolute()
            .top(px(2.))
            .right(px(4.))
            .flex()
            .gap(px(2.))
            .p(px(2.))
            .rounded(px(RADIUS_SMALL))
            // 70 % of the terminal color behind all buttons, so they read over text.
            .bg(rgba((theme.tokens.terminal << 8) | 0xb3))
            .opacity(if focused || popup_open { 1. } else { 0. })
            .group_hover(group, |style| style.opacity(1.))
            .child(button(Axis::Horizontal, "Split right", "splitRightClaude", SPLIT_RIGHT_ICON))
            .child(button(Axis::Vertical, "Split down", "splitDownClaude", SPLIT_DOWN_ICON))
            .child(div().w(px(1.)).h(theme.ui(14.)).bg(rgb(theme.tokens.line)).self_center())
            .child(
                div()
                    .id(("Close pane", index))
                    .flex_none()
                    .size(theme.ui(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(RADIUS_SMALL - 2.))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgba((danger << 8) | 0x24)))
                    .tooltip(hover_card::tooltip(close_card))
                    .child(svg().path(CLOSE_ICON).size(theme.ui(14.)).text_color(rgba((danger << 8) | 0xc7)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                            this.confirm_close(vec![leaf], window, cx);
                            window.prevent_default();
                            cx.stop_propagation();
                        }),
                    ),
            )
    }

    /// Open the Claude / Terminal popup of pane `leaf`'s split button at `position`.
    fn open_pane_menu(
        &mut self,
        leaf: NodeId,
        axis: Axis,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        self.pane_menu = Some(OpenMenu::new(menus::split_menu(leaf, axis), position, focus));
        cx.notify();
    }

    pub(super) fn render_active_tab(&self, animate: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = theme(cx).clone();
        let Some(tab) = self.workspace.active_tab() else {
            return div()
                .flex_1()
                .w_full()
                .flex()
                .flex_col()
                .gap_3()
                .items_center()
                .justify_center()
                .text_sm()
                .child(
                    div()
                        .id("empty-claude")
                        .w(theme.ui(220.))
                        .h(theme.ui(40.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(RADIUS))
                        .bg(rgb(theme.tokens.accent))
                        .text_color(rgb(theme.tokens.on_accent))
                        .font_weight(FontWeight::SEMIBOLD)
                        .cursor_pointer()
                        .child("New Claude session")
                        .on_click(cx.listener(|this, event: &gpui::ClickEvent, window, cx| {
                            this.new_pressed(true, event.position(), window, cx)
                        })),
                )
                .child(
                    div()
                        .id("empty-shell")
                        .w(theme.ui(220.))
                        .h(theme.ui(40.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(RADIUS))
                        .border_1()
                        .border_color(rgb(theme.tokens.line))
                        .text_color(rgb(theme.tokens.muted))
                        .cursor_pointer()
                        .hover(|style| style.text_color(rgb(theme.tokens.text)).bg(rgb(theme.tokens.raised)))
                        .child("New shell")
                        .on_click(cx.listener(|this, event: &gpui::ClickEvent, window, cx| {
                            this.new_pressed(false, event.position(), window, cx)
                        })),
                )
                .into_any_element();
        };
        if tab.settings {
            return match self.settings.clone() {
                Some(page) => div().id("tab-content").flex_1().min_h_0().w_full().child(page).into_any_element(),
                None => div().flex_1().into_any_element(),
            };
        }
        let multiple = tab.root.leaves().len() > 1;
        let layout = tab.root.layout(self.final_region());
        let drawn = self.drawn_rects();
        let animating = self.pane_anim.is_some() || self.panel_anim.is_some();
        let entering = self.pane_anim.as_ref().and_then(|anim| anim.entering);
        let size = self.content_bounds.size;
        let dragging_pane = cx.has_active_drag();
        div()
            .id("tab-content")
            .relative()
            .flex_1()
            .min_h_0()
            .w_full()
            // The entering pane slides in from beyond the tab's edge.
            .when(animating, |content| content.overflow_hidden())
            .child({
                let view = cx.entity();
                let resized = move |bounds: Bounds<Pixels>, _: &mut Window, cx: &mut App| {
                    view.update(cx, |this, _| {
                        // A window resize ends a split or close animation at its end.
                        if this.content_bounds.size != bounds.size {
                            this.cancel_pane_anim();
                        }
                        this.content_bounds = bounds
                    })
                };
                canvas(resized, |_, _, _, _| {}).absolute().size_full()
            })
            .children(layout.into_iter().zip(drawn).enumerate().filter_map(|(index, ((leaf, end), (_, rect)))| {
                let view = self.slots.get(&leaf)?.view.clone();
                let focused = leaf == tab.focused;
                let title = self.workspace.leaf_name(leaf).map_or_else(|| view.read(cx).title(), String::from);
                let status = self.workspace.leaf_status(leaf, self.sidebar.read(cx).sessions()).cloned();
                let title = if status.is_some() { without_claude_glyph(&title).to_string() } else { title };
                let unseen = self.unseen_of(leaf);
                let offline = self.leaf_offline(leaf);
                let container = self.workspace.leaves.get(&leaf).is_some_and(|entry| entry.launch.container.is_some());
                let host = self.workspace.leaves.get(&leaf).and_then(|leaf| leaf.host.remote_name().map(String::from));
                let dir = self.leaf_dir_badge(leaf, cx);
                let project = self.leaf_project_color(leaf, cx);
                let git_chip = self.git_chip_of(leaf, ("git-chip-pane", index), &theme, cx);
                let folder = self
                    .workspace
                    .leaves
                    .get(&leaf)
                    .map(|entry| display_path(&entry.launch.cwd, std::env::home_dir().as_deref()));
                let last_prompt_settings = cx.global::<SettingsStore>().settings().last_prompt;
                let last_prompt = if last_prompt_settings.enabled {
                    self.workspace.leaf_last_prompt(leaf, self.sidebar.read(cx).sessions()).map(str::to_string)
                } else {
                    None
                };
                // While a pane is being dragged, an overlay takes the pointer from the terminal and tracks the drop.
                let overlay = dragging_pane.then(|| {
                    div()
                        .id(("pane-drop", index))
                        .absolute()
                        .inset_0()
                        .occlude()
                        .on_drag_move(cx.listener(move |this, event: &DragMoveEvent<DraggedPane>, _, cx| {
                            this.drag_over_pane(leaf, event, cx)
                        }))
                        .on_drop(cx.listener(move |this, dragged: &DraggedPane, window, cx| {
                            this.drop_pane(dragged.leaf, leaf, window, cx)
                        }))
                });
                // The entering pane keeps its final size and moves; every other pane's card follows its drawn rectangle.
                let entering_axis = entering.filter(|(new, _)| *new == leaf).map(|(_, axis)| axis);
                let rect = entering_axis.map_or(rect, |axis| split_tree::slide_rect(rect, end, axis));
                Some(
                    div()
                        .absolute()
                        .left(relative(rect.x))
                        .top(relative(rect.y))
                        .w(relative(rect.w))
                        .h(relative(rect.h))
                        .p(px(PANE_GAP / 2.))
                        .child(
                            // A rounded card; the terminal sits inside its padding, clear of the corners. While a split
                            // or close animates, a surviving card resizes and clips its content, which keeps its final
                            // size (one resize, not one per frame); the entering card is already at its final size.
                            div()
                                .size_full()
                                .group(format!("pane-card-{index}"))
                                .p(px(CARD_PAD))
                                .rounded(px(RADIUS_LARGE))
                                .bg(rgb(theme.tokens.terminal))
                                .border_2()
                                .border_color(rgb(if focused && multiple {
                                    theme.tokens.accent
                                } else {
                                    theme.tokens.line
                                }))
                                .when(animating && entering_axis.is_none(), |card| card.overflow_hidden())
                                .child({
                                    let content = div().flex().flex_col();
                                    let content = if animating && entering_axis.is_none() {
                                        let [left, top, w, h] =
                                            content_frame(end, size.width.into(), size.height.into());
                                        content.absolute().left(px(left)).top(px(top)).w(px(w)).h(px(h))
                                    } else {
                                        content.size_full()
                                    };
                                    content
                                        .when(multiple, |pane| {
                                            pane.child(pane_header(
                                                index, leaf, title, status, unseen, offline, animate, container, host,
                                                dir, folder, project, git_chip, &theme,
                                            ))
                                        })
                                        .when_some(last_prompt, |pane, text| {
                                            pane.child(prompt_card(
                                                text,
                                                multiple,
                                                last_prompt_settings.size,
                                                last_prompt_settings.color,
                                                &theme,
                                            ))
                                        })
                                        .child(
                                            div()
                                                .relative()
                                                .flex_1()
                                                .min_h_0()
                                                .child(view)
                                                .child(self.split_buttons(leaf, index, focused, &theme, cx)),
                                        )
                                }),
                        )
                        .children(overlay),
                )
            }))
            .children(self.pane_drop_preview(tab, &theme, cx))
            .children(self.render_git_panel(&theme, cx))
            .children(tab.root.dividers(self.drawn_region()).into_iter().map(|divider| {
                let handle = div().id(("divider", divider.split as usize)).absolute();
                let handle = match divider.axis {
                    Axis::Horizontal => handle
                        .left(relative(divider.area.x + divider.area.w * divider.ratio))
                        .top(relative(divider.area.y))
                        .h(relative(divider.area.h))
                        .w(px(6.))
                        .ml(px(-3.))
                        .cursor(CursorStyle::ResizeLeftRight),
                    Axis::Vertical => handle
                        .top(relative(divider.area.y + divider.area.h * divider.ratio))
                        .left(relative(divider.area.x))
                        .w(relative(divider.area.w))
                        .h(px(6.))
                        .mt(px(-3.))
                        .cursor(CursorStyle::ResizeUpDown),
                };
                handle.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        this.dragging = Some(divider);
                        cx.stop_propagation();
                    }),
                )
            }))
            .on_mouse_move(cx.listener(Self::drag_divider))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.dragging.take().is_some() {
                        this.save(cx);
                    }
                }),
            )
            .into_any_element()
    }
}

/// Your last prompt in a card above a Claude pane's terminal: a fixed two lines, clipped with an ellipsis, on a
/// tint of `color` (the theme's accent when unset) with a rounded bar of it inset on the left (GPUI clips to a
/// rectangle, so a bar at the edge would poke out of the rounded corners). `size` is the text size.
fn prompt_card(text: String, multiple: bool, size: f32, color: Option<u32>, theme: &Theme) -> Div {
    let color = color.unwrap_or(theme.tokens.accent);
    let line = (size * 1.38).round();
    div()
        .relative()
        .flex_none()
        .h(theme.ui(line * 2. + 20.))
        .mb(px(6.))
        .when(multiple, |card| card.mt(px(6.)))
        .px(theme.ui(14.))
        .py(theme.ui(9.))
        .flex()
        .items_center()
        .gap(theme.ui(10.))
        .overflow_hidden()
        .rounded(px(RADIUS_SMALL))
        .bg(rgb(mix(color, theme.tokens.surface, 0.18)))
        .border_1()
        .border_color(rgb(mix(color, theme.tokens.line, 0.55)))
        .child(div().absolute().left(px(5.)).top(px(8.)).bottom(px(8.)).w(px(3.)).rounded_full().bg(rgb(color)))
        .child(svg().path(PROMPT_ICON).flex_none().size(theme.ui(size)).text_color(rgb(color)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(theme.ui(size))
                .line_height(theme.ui(line))
                .text_color(rgb(theme.tokens.text))
                .line_clamp(2)
                .text_ellipsis()
                .child(text),
        )
}

/// A split pane's header: filled like its tab, with the session's status in words.
#[allow(clippy::too_many_arguments)]
fn pane_header(
    index: usize,
    leaf: NodeId,
    title: String,
    status: Option<SessionStatus>,
    unseen: Unseen,
    offline: bool,
    animate: bool,
    container: bool,
    host: Option<String>,
    dir: Option<DirBadge>,
    folder: Option<String>,
    project: Option<u32>,
    git_chip: Option<gpui::Stateful<Div>>,
    theme: &Theme,
) -> impl IntoElement {
    let tone = leaf_tone(status.as_ref(), unseen, offline);
    let colors = tone.map(|tone| theme.tone(tone));
    let pin = (!offline && status_tone(status.as_ref(), unseen) == Tone::Unread)
        .then(|| pin_icon(colors.map_or(theme.tokens.text, |colors| colors.dot), theme));
    let sweep = tone.is_some_and(|tone| sweeping(tone, animate, true));
    let card = HoverCard {
        title: title.clone(),
        status: tone.map(|tone| tone_word(tone).to_string()),
        dir: dir.clone(),
        container,
        host: host.clone(),
        folder,
    };
    div()
        .id(("pane-header", index))
        .tooltip(hover_card::tooltip(card))
        .on_drag(DraggedPane { leaf, title: title.clone() }, |dragged: &DraggedPane, _offset, _window, cx: &mut App| {
            cx.new(|_| dragged.clone())
        })
        .relative()
        .flex_none()
        .h(theme.ui(28.))
        .mb(theme.ui(4.))
        .when(project.is_some(), |header| header.pl(theme.ui(16.)))
        .when(project.is_none(), |header| header.pl_3())
        .pr_3()
        .flex()
        .items_center()
        .gap_2()
        .rounded(px(RADIUS_SMALL))
        .text_xs()
        .font_weight(FontWeight::SEMIBOLD)
        .bg(rgb(colors.and_then(|colors| colors.fill).unwrap_or(theme.tokens.raised)))
        .text_color(rgb(colors.map_or(theme.tokens.text, |colors| colors.ink)))
        .when(sweep, |header| header.child(working_sweep(("sweep-pane", index), px(RADIUS_SMALL), theme)))
        .when_some(project, |header, color| header.child(side_line(color, theme)))
        .children(pin)
        .child(div().flex_1().min_w_0().truncate().child(title))
        .children(git_chip)
        .when(container, |header| {
            header.child(container_icon(colors.map_or(theme.tokens.text, |colors| colors.ink), theme))
        })
        .when(host.is_some(), |header| {
            header.child(ssh_icon(colors.map_or(theme.tokens.text, |colors| colors.ink), theme))
        })
        .children(dir.map(|badge| dir_badge(&badge, theme)))
}
