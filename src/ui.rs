//! OUTNO look & feel, shared by the compiler, the setup wizard and the uninstaller.
//!
//! Light, Windows 11-flavoured theme: soft grey canvas, white cards, rounded
//! inputs with a blue focus ring, a branded header and a Back / Next / Cancel
//! footer. On Windows the system Segoe UI font is used when it is available.

use eframe::egui::{
    self, Color32, FontData, FontDefinitions, FontFamily, Frame, Margin, RichText, Rounding, Stroke,
    TextureHandle, TextureOptions, Vec2,
};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

/// Accent colour (0xRRGGBB). Setups can override it from their manifest.
static ACCENT_RGB: AtomicU32 = AtomicU32::new(0x0A64D6);

/// Change the accent colour. Call before `setup` so the theme picks it up.
pub fn set_accent(rgb: [u8; 3]) {
    ACCENT_RGB.store(u32::from_be_bytes([0, rgb[0], rgb[1], rgb[2]]), Ordering::Relaxed);
}

fn mix(c: Color32, target: u8, t: f32) -> Color32 {
    let m = |v: u8| (v as f32 + (target as f32 - v as f32) * t).round() as u8;
    Color32::from_rgb(m(c.r()), m(c.g()), m(c.b()))
}

pub fn accent() -> Color32 {
    let [_, r, g, b] = ACCENT_RGB.load(Ordering::Relaxed).to_be_bytes();
    Color32::from_rgb(r, g, b)
}
/// Darker accent for pressed states and button borders.
pub fn accent_press() -> Color32 {
    mix(accent(), 0, 0.3)
}
pub fn banner_top() -> Color32 {
    mix(accent(), 255, 0.1)
}
pub fn banner_bottom() -> Color32 {
    mix(accent(), 0, 0.45)
}

// ── Brand palette (taken from the OUTNO icon) ────────────────────────────────
pub const DEFAULT_ACCENT: [u8; 3] = [10, 100, 214];
pub const BG: Color32 = Color32::from_rgb(243, 244, 246);
pub const CARD: Color32 = Color32::WHITE;
pub const BORDER: Color32 = Color32::from_rgb(218, 221, 227);
pub const TEXT: Color32 = Color32::from_rgb(31, 35, 40);
pub const MUTED: Color32 = Color32::from_rgb(95, 102, 114);
pub const ERROR: Color32 = Color32::from_rgb(196, 43, 28);
pub const WARNING: Color32 = Color32::from_rgb(157, 93, 0);
pub const SUCCESS: Color32 = Color32::from_rgb(16, 124, 16);

pub const BRAND: &str = "OUTNO";
pub const TAGLINE: &str = "Lightweight setup creator";

/// 256 px icon, embedded so every generated setup / uninstaller carries it.
const ICON_PNG: &[u8] = include_bytes!("../assets/outno-256.png");

const BOLD: &str = "bold";

#[derive(PartialEq, Clone, Copy)]
pub enum Nav {
    None,
    Back,
    Next,
    Cancel,
}

/// Brand assets that need a live egui context (textures).
#[derive(Clone)]
pub struct Brand {
    pub icon: TextureHandle,
    /// Custom side-banner image for the Welcome / Finished pages.
    pub wizard: Option<TextureHandle>,
    /// Custom image in the top-right corner of the page header.
    pub header: Option<TextureHandle>,
}

impl Brand {
    pub fn image(&self, size: f32) -> egui::Image<'static> {
        egui::Image::from_texture((self.icon.id(), Vec2::splat(size))).fit_to_exact_size(Vec2::splat(size))
    }
}

/// Decode a PNG into a texture.
pub fn load_png(ctx: &egui::Context, name: &str, png: &[u8]) -> Option<TextureHandle> {
    let d = eframe::icon_data::from_png_bytes(png).ok()?;
    let img = egui::ColorImage::from_rgba_unmultiplied([d.width as usize, d.height as usize], &d.rgba);
    Some(ctx.load_texture(name, img, TextureOptions::LINEAR))
}

/// Largest size with the texture's aspect ratio that fits in `max`.
fn fit(tex: &TextureHandle, max: Vec2) -> Vec2 {
    let s = tex.size_vec2();
    let k = (max.x / s.x).min(max.y / s.y);
    s * k
}

fn icon_data() -> egui::IconData {
    eframe::icon_data::from_png_bytes(ICON_PNG).unwrap_or_default()
}

pub fn window_options(title: &str, size: [f32; 2], resizable: bool) -> eframe::NativeOptions {
    let mut vp = egui::ViewportBuilder::default()
        .with_title(title)
        .with_app_id("outno")
        .with_icon(Arc::new(icon_data()))
        .with_inner_size(size)
        .with_resizable(resizable)
        .with_maximize_button(resizable)
        .with_drag_and_drop(true);
    if resizable {
        vp = vp.with_min_inner_size([size[0] * 0.85, size[1] * 0.8]);
    }
    eframe::NativeOptions { viewport: vp, centered: true, ..Default::default() }
}

/// Install fonts + theme and upload the brand texture. Call once from the app creator.
pub fn setup(ctx: &egui::Context) -> Brand {
    install_fonts(ctx);
    apply_theme(ctx);
    let icon = icon_data();
    let img = egui::ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
    Brand { icon: ctx.load_texture("outno-icon", img, TextureOptions::LINEAR), wizard: None, header: None }
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let default_prop = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();

    let read = |name: &str| -> Option<Vec<u8>> {
        let windir = std::env::var_os("WINDIR")?;
        std::fs::read(std::path::Path::new(&windir).join("Fonts").join(name)).ok()
    };

    let mut bold_family = default_prop.clone();
    if let Some(regular) = read("segoeui.ttf") {
        fonts.font_data.insert("segoe".into(), FontData::from_owned(regular));
        if let Some(p) = fonts.families.get_mut(&FontFamily::Proportional) {
            p.insert(0, "segoe".into());
        }
        bold_family.insert(0, "segoe".into());
    }
    if let Some(semi) = read("seguisb.ttf").or_else(|| read("segoeuib.ttf")) {
        fonts.font_data.insert("segoe-semibold".into(), FontData::from_owned(semi));
        bold_family.insert(0, "segoe-semibold".into());
    }
    // The family must always exist, otherwise egui panics when it is used.
    fonts.families.insert(FontFamily::Name(BOLD.into()), bold_family);
    ctx.set_fonts(fonts);
}

fn apply_theme(ctx: &egui::Context) {
    let mut v = egui::Visuals::light();
    v.override_text_color = Some(TEXT);
    v.panel_fill = BG;
    v.window_fill = CARD;
    v.window_rounding = Rounding::same(10.0);
    v.window_stroke = Stroke::new(1.0_f32, BORDER);
    v.window_shadow = egui::epaint::Shadow {
        offset: egui::vec2(0.0, 8.0),
        blur: 24.0,
        spread: 0.0,
        color: Color32::from_black_alpha(40),
    };
    v.popup_shadow = egui::epaint::Shadow {
        offset: egui::vec2(0.0, 4.0),
        blur: 12.0,
        spread: 0.0,
        color: Color32::from_black_alpha(30),
    };
    v.menu_rounding = Rounding::same(6.0);
    v.extreme_bg_color = Color32::WHITE; // text-edit background
    v.faint_bg_color = Color32::from_rgb(248, 249, 250);
    v.hyperlink_color = accent();
    v.selection.bg_fill = Color32::from_rgb(204, 226, 252);
    v.selection.stroke = Stroke::new(1.5_f32, accent()); // focus ring on text edits
    v.text_cursor.stroke = Stroke::new(1.5_f32, accent());

    let r = Rounding::same(5.0);
    v.widgets.noninteractive.rounding = r;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, BORDER);
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, TEXT);

    v.widgets.inactive.rounding = r;
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(251, 251, 252);
    v.widgets.inactive.bg_fill = Color32::from_rgb(236, 238, 241);
    v.widgets.inactive.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(200, 204, 211));
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, TEXT);

    v.widgets.hovered.rounding = r;
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(240, 245, 252);
    v.widgets.hovered.bg_fill = Color32::from_rgb(225, 236, 250);
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(120, 160, 220));
    v.widgets.hovered.fg_stroke = Stroke::new(1.5_f32, TEXT);
    v.widgets.hovered.expansion = 0.0;

    v.widgets.active.rounding = r;
    v.widgets.active.weak_bg_fill = Color32::from_rgb(220, 232, 248);
    v.widgets.active.bg_fill = Color32::from_rgb(210, 226, 248);
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, accent());
    v.widgets.active.fg_stroke = Stroke::new(1.5_f32, TEXT);
    v.widgets.active.expansion = 0.0;

    v.widgets.open = v.widgets.active;
    ctx.set_visuals(v);

    ctx.style_mut(|s| {
        s.spacing.button_padding = egui::vec2(12.0, 5.0);
        s.spacing.item_spacing = egui::vec2(8.0, 7.0);
        s.spacing.interact_size.y = 26.0;
        s.spacing.icon_width = 16.0;
        s.spacing.icon_width_inner = 9.0;
        s.spacing.scroll = egui::style::ScrollStyle::thin();
        s.text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(14.0));
        s.text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(14.0));
        s.text_styles.insert(egui::TextStyle::Small, egui::FontId::proportional(12.0));
        s.text_styles.insert(egui::TextStyle::Monospace, egui::FontId::monospace(12.5));
    });
}

// ── Small building blocks ────────────────────────────────────────────────────

/// Heading text in the semibold family (falls back to the regular font).
pub fn bold(text: impl Into<String>, size: f32) -> RichText {
    RichText::new(text).family(FontFamily::Name(BOLD.into())).size(size)
}

pub fn muted(text: impl Into<String>) -> RichText {
    RichText::new(text).color(MUTED)
}

pub fn button(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(text.into()).min_size(egui::vec2(88.0, 30.0))
}

/// Filled accent button for the main action of a screen.
pub fn primary_button(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(bold(text, 14.0).color(Color32::WHITE))
        .fill(accent())
        .stroke(Stroke::new(1.0_f32, accent_press()))
        .min_size(egui::vec2(96.0, 30.0))
}

/// White rounded card with a title, used to group form sections.
pub fn card<R>(ui: &mut egui::Ui, title: &str, subtitle: &str, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    Frame::none()
        .fill(CARD)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .rounding(Rounding::same(8.0))
        .inner_margin(Margin::symmetric(16.0, 14.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(bold(title, 15.0));
            if !subtitle.is_empty() {
                ui.label(muted(subtitle).size(12.5));
            }
            ui.add_space(6.0);
            add(ui)
        })
        .inner
}

/// White box with a thin border (license text, summaries, logs).
pub fn inset_frame() -> Frame {
    Frame::none()
        .fill(Color32::WHITE)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .rounding(Rounding::same(6.0))
        .inner_margin(Margin::same(10.0))
}

fn vgradient(p: &egui::Painter, r: egui::Rect, top: Color32, bottom: Color32) {
    let mut m = egui::Mesh::default();
    m.colored_vertex(r.left_top(), top);
    m.colored_vertex(r.right_top(), top);
    m.colored_vertex(r.left_bottom(), bottom);
    m.colored_vertex(r.right_bottom(), bottom);
    m.add_triangle(0, 1, 2);
    m.add_triangle(1, 3, 2);
    p.add(m);
}

// ── Wizard chrome ────────────────────────────────────────────────────────────

/// White page header: bold title, subtitle and the OUTNO icon on the right.
pub fn header(ctx: &egui::Context, brand: &Brand, title: &str, subtitle: &str) {
    egui::TopBottomPanel::top("header")
        .exact_height(66.0)
        .frame(
            Frame::none()
                .fill(Color32::WHITE)
                .inner_margin(Margin { left: 22.0, right: 16.0, top: 10.0, bottom: 10.0 })
                .stroke(Stroke::new(1.0_f32, BORDER)),
        )
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.add_space(2.0);
                    ui.label(bold(title, 16.0));
                    ui.label(muted(subtitle).size(13.0));
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match &brand.header {
                    Some(t) => {
                        ui.add(egui::Image::from_texture((t.id(), fit(t, egui::vec2(120.0, 48.0)))));
                    }
                    None => {
                        ui.add(brand.image(44.0));
                    }
                });
            });
        });
}

/// Bottom bar:  OUTNO            [< Back] [Next >]  [Cancel]
/// `back` / `cancel`: None = hidden, Some((enabled, label)).
/// Enter activates Next, Escape activates Cancel.
pub fn footer(
    ctx: &egui::Context,
    back: Option<(bool, &str)>,
    next_label: &str,
    next_enabled: bool,
    cancel: Option<(bool, &str)>,
) -> Nav {
    let mut nav = Nav::None;
    egui::TopBottomPanel::bottom("footer")
        .exact_height(56.0)
        .frame(Frame::none().fill(BG).inner_margin(Margin::symmetric(16.0, 12.0)).stroke(Stroke::new(1.0_f32, BORDER)))
        .show(ctx, |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(bold(BRAND, 12.0).color(Color32::from_rgb(150, 156, 166)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some((en, label)) = cancel {
                        if ui.add_enabled(en, button(label)).clicked() {
                            nav = Nav::Cancel;
                        }
                        ui.add_space(8.0);
                    }
                    if ui.add_enabled(next_enabled, primary_button(next_label)).clicked() {
                        nav = Nav::Next;
                    }
                    if let Some((en, label)) = back {
                        if ui.add_enabled(en, button(label)).clicked() {
                            nav = Nav::Back;
                        }
                    }
                });
            });
        });

    if nav == Nav::None {
        let (enter, esc) = ctx.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
        if enter && next_enabled {
            nav = Nav::Next;
        } else if esc && cancel.is_some_and(|c| c.0) {
            nav = Nav::Cancel;
        }
    }
    nav
}

/// Gradient side banner for the Welcome / Finished pages, or the custom wizard image.
/// `version_line` is shown under the app name (already translated).
pub fn side_banner(ctx: &egui::Context, brand: &Brand, app_name: &str, version_line: &str) {
    egui::SidePanel::left("banner")
        .exact_width(172.0)
        .resizable(false)
        .show_separator_line(false)
        .frame(Frame::none())
        .show(ctx, |ui| {
            let rect = ui.max_rect();
            if let Some(t) = &brand.wizard {
                // Cover the banner, cropping the image centrally if the aspect differs.
                let (img, r) = (t.size_vec2(), rect.size());
                let k = (r.x / img.x).max(r.y / img.y);
                let (u, v) = (r.x / (img.x * k), r.y / (img.y * k));
                let uv = egui::Rect::from_min_max(
                    egui::pos2((1.0 - u) / 2.0, (1.0 - v) / 2.0),
                    egui::pos2((1.0 + u) / 2.0, (1.0 + v) / 2.0),
                );
                ui.painter().image(t.id(), rect, uv, Color32::WHITE);
                return;
            }
            vgradient(ui.painter(), rect, banner_top(), banner_bottom());
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(rect.shrink(14.0)), |ui| {
                ui.add_space(46.0);
                ui.vertical_centered(|ui| {
                    ui.add(brand.image(104.0));
                    ui.add_space(14.0);
                    ui.label(bold(app_name, 17.0).color(Color32::WHITE));
                    if !version_line.is_empty() {
                        ui.label(RichText::new(version_line).size(12.5).color(Color32::from_white_alpha(200)));
                    }
                });
            });
        });
}

/// Dims the window and swallows clicks so a dialog on top behaves modally.
pub fn modal_backdrop(ctx: &egui::Context) {
    egui::Area::new(egui::Id::new("modal-backdrop"))
        .order(egui::Order::PanelResizeLine)
        .fixed_pos(egui::Pos2::ZERO)
        .show(ctx, |ui| {
            let r = ctx.screen_rect();
            ui.painter().rect_filled(r, 0.0, Color32::from_black_alpha(70));
            ui.allocate_rect(r, egui::Sense::click_and_drag());
        });
}

/// Centered yes/no dialog. Returns Some(true) for Yes, Some(false) for No.
pub fn confirm_dialog(ctx: &egui::Context, title: &str, lines: &[&str], yes: &str, no: &str) -> Option<bool> {
    modal_backdrop(ctx);
    let mut answer = None;
    egui::Window::new(title)
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .fixed_size([340.0, 0.0])
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .frame(Frame::window(&ctx.style()).inner_margin(Margin::same(18.0)))
        .show(ctx, |ui| {
            ui.label(bold(title, 15.0));
            ui.add_space(6.0);
            for l in lines {
                ui.label(*l);
            }
            ui.add_space(12.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add(button(no)).clicked() {
                    answer = Some(false);
                }
                if ui.add(primary_button(yes)).clicked() {
                    answer = Some(true);
                }
            });
        });
    if answer.is_none() {
        let (enter, esc) = ctx.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
        if enter {
            answer = Some(true);
        } else if esc {
            answer = Some(false);
        }
    }
    answer
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
