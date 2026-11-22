//! Classic Inno Setup look: grey wizard body, white header strip, flat buttons,
//! Back / Next / Cancel footer, dark-blue banner on the Welcome / Finish pages.

use eframe::egui::{self, Color32, Frame, Margin, RichText, Rounding, Stroke};

pub const GRAY: Color32 = Color32::from_rgb(240, 240, 240);
pub const BANNER: Color32 = Color32::from_rgb(0, 51, 102);
const BORDER: Color32 = Color32::from_rgb(173, 173, 173);
const BLUE: Color32 = Color32::from_rgb(0, 120, 215);

#[derive(PartialEq, Clone, Copy)]
pub enum Nav {
    None,
    Back,
    Next,
    Cancel,
}

pub fn window_options(title: &str, size: [f32; 2]) -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size(size)
            .with_resizable(false)
            .with_maximize_button(false),
        centered: true,
        ..Default::default()
    }
}

pub fn apply_classic(ctx: &egui::Context) {
    let mut v = egui::Visuals::light();
    v.panel_fill = GRAY;
    v.window_fill = GRAY;
    v.window_rounding = Rounding::ZERO;
    v.window_stroke = Stroke::new(1.0, BORDER);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.rounding = Rounding::ZERO;
    }
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(225, 225, 225);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(229, 241, 251);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0, BLUE);
    v.widgets.active.weak_bg_fill = Color32::from_rgb(204, 228, 247);
    v.widgets.active.bg_stroke = Stroke::new(1.0, Color32::from_rgb(0, 84, 153));
    v.selection.bg_fill = BLUE;
    ctx.set_visuals(v);

    let mut style = (*ctx.style()).clone();
    style.spacing.button_padding = egui::vec2(14.0, 4.0);
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    ctx.set_style(style);
}

fn button(text: &str) -> egui::Button<'_> {
    egui::Button::new(text).min_size(egui::vec2(80.0, 26.0))
}

/// White strip with bold title + subtitle (Inno's wizard page header).
pub fn header(ctx: &egui::Context, title: &str, subtitle: &str) {
    egui::TopBottomPanel::top("header")
        .exact_height(60.0)
        .frame(Frame::none().fill(Color32::WHITE).inner_margin(Margin::symmetric(18.0, 10.0)))
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            ui.label(RichText::new(title).strong().size(15.0));
            ui.label(format!("   {subtitle}"));
        });
}

/// Bottom button bar:  [< Back] [Next >]   [Cancel]
/// `back` / `cancel`: None = hidden, Some(enabled).
pub fn footer(
    ctx: &egui::Context,
    back: Option<bool>,
    next_label: &str,
    next_enabled: bool,
    cancel: Option<bool>,
) -> Nav {
    let mut nav = Nav::None;
    egui::TopBottomPanel::bottom("footer")
        .exact_height(50.0)
        .frame(Frame::none().fill(GRAY).inner_margin(Margin::symmetric(14.0, 11.0)))
        .show(ctx, |ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(en) = cancel {
                    if ui.add_enabled(en, button("Cancel")).clicked() {
                        nav = Nav::Cancel;
                    }
                    ui.add_space(10.0);
                }
                if ui.add_enabled(next_enabled, button(next_label)).clicked() {
                    nav = Nav::Next;
                }
                if let Some(en) = back {
                    if ui.add_enabled(en, button("< Back")).clicked() {
                        nav = Nav::Back;
                    }
                }
            });
        });
    nav
}

/// Dark-blue side banner for the Welcome / Finished pages.
pub fn side_banner(ctx: &egui::Context, app_name: &str) {
    egui::SidePanel::left("banner")
        .exact_width(164.0)
        .resizable(false)
        .frame(Frame::none().fill(BANNER).inner_margin(Margin::same(12.0)))
        .show(ctx, |ui| {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                let initial = app_name.chars().next().unwrap_or('S').to_uppercase().to_string();
                ui.label(RichText::new(initial).size(64.0).strong().color(Color32::WHITE));
                ui.add_space(8.0);
                ui.label(RichText::new(app_name).size(16.0).color(Color32::from_rgb(200, 220, 245)));
            });
        });
}

pub fn fmt_size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= 1024.0 * MB {
        format!("{:.1} GB", b / (1024.0 * MB))
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else {
        format!("{:.0} KB", (b / 1024.0).max(1.0))
    }
}
