// SPDX-FileCopyrightText: MailCraft contributors
// SPDX-License-Identifier: MIT OR Apache-2.0
//! The phone interface, modelled on Microsoft Outlook for Android: a top app bar with a menu
//! (hamburger) button, a navigation drawer of the folders and accounts, a single column of
//! messages, a full screen reading view, a compose button that floats at the lower right, and a
//! tab bar at the foot that switches between Mail, Calendar, People and Tasks.
//!
//! The desktop window keeps its three columns; the phone interface keeps the folders in the drawer
//! and shows one thing at a time. It is entered by the window's own setting
//! ([`crate::UiState::phone_layout`]) or by a window under 600 points wide.

use egui::{Color32, Rect, Sense, Stroke, Vec2, pos2};
use serde_json::json;

use crate::theme::{Tokens, regular, semibold};
use crate::{icons, MailApp};
use mailcraft_engine::Module;

/// Whether the phone interface is in view: the window's own setting, or a window too narrow for
/// the desktop's three columns.
pub fn active(app: &MailApp, ui: &egui::Ui) -> bool {
    app.ui.phone_layout || ui.available_width() < 600.0
}

/// The app bar's height, and the tab bar's height.
const TOP_H: f32 = 52.0;
const TABS_H: f32 = 56.0;

/// The drawer, the reading view and the body, laid out as one thing on the screen at a time.
pub fn show(app: &mut MailApp, ui: &mut egui::Ui) {
    let t = Tokens::get(ui.ctx());
    let ctx = ui.ctx().clone();
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, 0.0, t.canvas);

    // The drawer is open until the owner closes it; it is kept in the frame, not in the settings.
    let drawer_id = egui::Id::unique("phone_drawer");
    let mut drawer = ctx.data_mut(|d| d.get_temp::<bool>(drawer_id)).unwrap_or(false);

    // A message being written, or a message read in full, is the one thing the body shows.
    let composing = app.session.compose_id().is_some();
    let module = app.session.view.module;
    let reading = module == Module::Mail && !composing && app.session.sel.focus().is_some();
    let folder = app.session.view.folder;

    // Top app bar.
    egui::Panel::top("phone_top")
        .frame(egui::Frame::NONE.fill(t.panel).stroke(Stroke::new(1.0, t.border)))
        .show(ui, |ui| top_bar(app, ui, &t, &mut drawer, reading));

    // The navigation drawer, when it is open, pushes the body to its right.
    if drawer {
        egui::Panel::left("phone_drawer")
            .resizable(false)
            .exact_size((rect.width() * 0.85).clamp(280.0, 340.0))
            .frame(egui::Frame::NONE.fill(t.panel).stroke(Stroke::new(1.0, t.border)))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                crate::nav::folder_pane(app, ui);
            });
    }

    // The tab bar at the foot is claimed *before* the body: the central panel is greedy and takes
    // whatever is left, so a panel made after it would get no room.
    egui::Panel::bottom("phone_tabs")
        .frame(egui::Frame::NONE.fill(t.panel).stroke(Stroke::new(1.0, t.border)))
        .show(ui, |ui| bottom_tabs(app, ui, &t, module));

    // The body: one thing at a time, in whatever the fixed panels left. The compose button is drawn
    // *after* the body (still inside this panel), so it sits in front of the message list and the
    // panel's own background.
    egui::CentralPanel::default().frame(egui::Frame::NONE.fill(t.surface)).show(ui, |ui| {
        body(app, ui, composing, reading, module);
        compose_button(app, ui, &t, module, composing, reading);
    });

    // Selecting a folder in the drawer leaves it (Outlook closes the drawer on a folder tap).
    if drawer && app.session.view.folder != folder {
        drawer = false;
    }
    if drawer {
        ctx.data_mut(|d| d.insert_temp(drawer_id, true));
    } else {
        ctx.data_mut(|d| d.remove_temp::<bool>(drawer_id));
    }
}

/// An icon that is a button: a square of `size` that paints its hover, draws the icon, and answers
/// a click. The icon is the theme's colour on the accent, as the module bar and the tabs do.
fn icon_button(
    ui: &mut egui::Ui,
    size: f32,
    icon: &str,
    color: Color32,
    accent: Color32,
    t: &Tokens,
) -> bool {
    let (r, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(r, 8.0, t.hover);
    }
    icons::paint(ui.painter(), r.shrink2(Vec2::splat((size - 24.0) / 2.0)), icon, color, accent);
    resp.clicked()
}

/// The top app bar: the menu (or back) button on the left, the folder's name beside it, and the
/// search, the send/receive and the overflow at the right.
fn top_bar(app: &mut MailApp, ui: &mut egui::Ui, t: &Tokens, drawer: &mut bool, reading: bool) {
    ui.set_height(TOP_H);
    ui.set_min_width(ui.available_width());
    let folder = app.session.view.folder;
    ui.horizontal(|ui| {
        // Left button: back to the list when a message is read, else the menu that opens the drawer.
        let left = icon_button(ui, 40.0, if reading { "back" } else { "menu" }, t.text, t.accent, t);
        if left {
            if reading {
                // Clear the selection: the reading view follows the last selected message, so an
                // empty selection returns to the list.
                app.session.sel.messages.clear();
            } else {
                *drawer = !*drawer;
            }
        }
        // The folder's name (translated; e.g. 收件箱). In the reading view it names the folder the
        // message is read from, as Outlook for Android does; the back arrow returns to the list.
        ui.add_space(6.0);
        let title = folder_label(app, folder);
        ui.label(egui::RichText::new(title).font(semibold(17.0)).color(t.text).strong());
        // The search, send/receive and the overflow, at the right.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.set_width(ui.available_width());
            if icon_button(ui, 40.0, "search", t.text, t.accent, t) {
                let _ = app.run("view.search", json!({}));
            }
            if icon_button(ui, 40.0, "sync", t.text, t.accent, t) {
                let _ = app.run("ui.sync", json!({}));
            }
            if icon_button(ui, 40.0, "more", t.text, t.accent, t) {
                let _ = app.run("ui.backstage", json!({"page": "options"}));
            }
        });
    });
}

/// The folder's name in the bar's title, in the interface language.
fn folder_label(app: &MailApp, folder: u64) -> String {
    match app.session.mailbox.folder(folder) {
        Some(f) => crate::nav::folder_name(f).to_string(),
        None => crate::i18n::t("Inbox").to_string(),
    }
}

/// The body of the phone interface: the one thing in view. A message being written takes the
/// screen; else, in the mail module, a message read in full, or the single column of messages; the
/// other modules show their own view.
fn body(app: &mut MailApp, ui: &mut egui::Ui, composing: bool, reading: bool, module: Module) {
    ui.set_min_width(ui.available_width());
    if composing {
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            crate::compose::show(app, ui);
        });
        return;
    }
    match module {
        Module::Mail => {
            if reading {
                // A message read in full; the bar's back button returns to the list.
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    crate::reading::show(app, ui);
                });
            } else {
                crate::mail_view::list_only(app, ui);
            }
        }
        Module::Calendar => crate::calendar_view::show(app, ui),
        Module::People => crate::people_view::show(app, ui),
        Module::Tasks => crate::tasks_view::show(app, ui),
    }
}

/// The tab bar at the foot: Mail, Calendar, People and Tasks, each an icon and its name; the one
/// in view is in the accent colour, with a short line under it.
///
/// The tabs share the whole width evenly, so the bar fills any phone's width and never leaves an
/// empty stretch at the right nor overflows on a narrow one: each tab takes `total / n` points, and
/// its icon and name are centred in that slice.
fn bottom_tabs(app: &mut MailApp, ui: &mut egui::Ui, t: &Tokens, module: Module) {
    ui.set_height(TABS_H);
    ui.set_min_width(ui.available_width());
    let tabs: [(Module, &str, &str); 4] = [
        (Module::Mail, "mail", "Mail"),
        (Module::Calendar, "calendar", "Calendar"),
        (Module::People, "people", "People"),
        (Module::Tasks, "tasks", "Tasks"),
    ];
    // Even slices of the whole width: one per tab, so the bar spans from edge to edge.
    let cell = ui.available_width() / tabs.len() as f32;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (m, icon, key) in tabs {
            let on = module == m;
            let color = if on { t.accent } else { t.text_dim };
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(cell, TABS_H - 6.0), Sense::click());
            // Centre the icon and the name on the tab's slice, not on the running cursor.
            let cx = rect.center().x;
            icons::paint(ui.painter(), Rect::from_center_size(pos2(cx, rect.min.y + 12.0), Vec2::splat(24.0)), icon, color, t.accent);
            ui.painter().text(pos2(cx, rect.min.y + 36.0), egui::Align2::CENTER_CENTER, crate::i18n::t(key), regular(11.0), color);
            if on {
                // A short accent line under the tab in view.
                ui.painter().line(
                    vec![pos2(cx - 14.0, rect.min.y + 2.0), pos2(cx + 14.0, rect.min.y + 2.0)],
                    Stroke::new(3.0, t.accent),
                );
            }
            if resp.clicked() {
                let cmd = match m {
                    Module::Mail => "view.mail",
                    Module::Calendar => "view.calendar",
                    Module::People => "view.people",
                    Module::Tasks => "view.tasks",
                };
                let _ = app.run(cmd, json!({}));
            }
        }
    });
}

/// The compose button: a round button in the accent, floating at the lower right above the tab bar.
/// It is hidden over the message being written and over a message read in full.
///
/// It is painted after the body (still inside the central panel), so it sits in front of the
/// message list and the panel's own background; it is anchored to a point just above the tab bar,
/// 24 points from the right edge.
fn compose_button(app: &mut MailApp, ui: &mut egui::Ui, t: &Tokens, module: Module, composing: bool, reading: bool) {
    if module != Module::Mail || composing || reading {
        return;
    }
    let size = 56.0;
    // Just above the tab bar (the central panel ends where the tab bar begins): 12 points of air,
    // 24 from the right.
    let cr = ui.max_rect();
    let center = pos2(cr.max.x - 24.0 - size / 2.0, cr.max.y - 12.0 - size / 2.0);
    let r = Rect::from_center_size(center, Vec2::splat(size));
    let resp = ui.interact(r, egui::Id::unique("phone_compose"), egui::Sense::click());
    if resp.hovered() {
        ui.ctx().request_repaint();
        ui.painter().circle(r.center(), size / 2.0, t.accent.linear_multiply(0.9), Stroke::NONE);
    }
    ui.painter().circle(r.center(), size / 2.0 - 1.0, t.accent, Stroke::NONE);
    icons::paint(ui.painter(), r.shrink2(Vec2::splat(16.0)), "new", t.on_accent, t.on_accent);
    if resp.clicked() {
        let _ = app.run("mail.new", json!({}));
    }
}
