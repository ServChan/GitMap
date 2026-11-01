//! Оформление: печатная транспортная схема. Сам интерфейс монохромный (бумага и тушь),
//! цвет есть только у веток. Углы прямые, тени не используются, разделители тонкие.
//! Контраст проверяется тестами внизу файла.

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontTweak, Shadow, Stroke,
    Theme, Visuals,
};
use eframe::epaint::text::VariationCoords;
use crate::layout::PALETTE_SIZE;
use std::sync::Arc;

pub struct Palette {
    pub paper: Color32,
    /// Наведение на строку.
    pub paper_hover: Color32,
    /// Основная линия, текст, выбранная строка.
    pub ink: Color32,
    /// Вторичный текст (не ниже 4.5:1 на бумаге).
    pub ink_weak: Color32,
    /// Декоративные линии.
    pub rule: Color32,
    /// Границы элементов управления (не ниже 3:1 на бумаге).
    pub rule_strong: Color32,
    /// Цвета веток (не ниже 4.5:1 на бумаге: ими же подписаны линии).
    pub lines: [Color32; PALETTE_SIZE],
    /// Текст на табличке цвета ветки (не ниже 4.5:1 к любому из `lines`).
    pub on_line: Color32,
    pub error: Color32,
}

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

pub const LIGHT: Palette = Palette {
    paper: hex(0xF2EEE3),
    paper_hover: hex(0xE7E2D3),
    ink: hex(0x1B1D21),
    ink_weak: hex(0x5B5A54),
    rule: hex(0xD3CDBC),
    rule_strong: hex(0x87826F),
    lines: [
        hex(0xC8102E), // красная
        hex(0x0B5CAD), // синяя
        hex(0x12743A), // зелёная
        hex(0x6B3FA0), // фиолетовая
        hex(0xA94B00), // оранжевая
        hex(0x006F70), // бирюзовая
        hex(0xA8146F), // пурпурная
        hex(0x774719), // коричневая
        hex(0x5C6600), // оливковая
        hex(0x3A567C), // стальная
    ],
    on_line: hex(0xFBF8F0),
    error: hex(0xA6171F),
};

pub const DARK: Palette = Palette {
    paper: hex(0x15181C),
    paper_hover: hex(0x20242A),
    ink: hex(0xEDE7D9),
    ink_weak: hex(0xA9A598),
    rule: hex(0x2E333A),
    rule_strong: hex(0x7D8086),
    lines: [
        hex(0xFF6F61),
        hex(0x62AEFF),
        hex(0x4FCC7D),
        hex(0xBB9DFF),
        hex(0xFFA552),
        hex(0x40D0C9),
        hex(0xFF7FC8),
        hex(0xD6A873),
        hex(0xC6D052),
        hex(0xA3B9D9),
    ],
    on_line: hex(0x12151A),
    error: hex(0xFF9A94),
};

pub fn palette(ui: &egui::Ui) -> &'static Palette {
    if ui.visuals().dark_mode { &DARK } else { &LIGHT }
}

pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

/// Узкое жирное начертание для крупных заголовков.
pub fn display() -> FontFamily {
    FontFamily::Name("display".into())
}

pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    ctx.set_visuals_of(Theme::Light, visuals(&LIGHT, false));
    ctx.set_visuals_of(Theme::Dark, visuals(&DARK, true));
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.interact_size.y = 28.0;
        style.spacing.icon_width = 16.0;
        style.spacing.icon_width_inner = 10.0;
        style.spacing.scroll.bar_width = 6.0;
        style.spacing.scroll.floating = true;
        for (text_style, font) in style.text_styles.iter_mut() {
            font.size = match text_style {
                egui::TextStyle::Heading => 24.0,
                egui::TextStyle::Small => 12.0,
                egui::TextStyle::Monospace => 13.0,
                _ => 14.0,
            };
        }
        if let Some(heading) = style.text_styles.get_mut(&egui::TextStyle::Heading) {
            heading.family = display();
        }
    });
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    let fallbacks = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let mut load = |name: &str, file: &str, axes: &[(&str, f32)]| -> bool {
        let Ok(bytes) = std::fs::read(format!(r"C:\Windows\Fonts\{file}")) else { return false };
        let tweak = FontTweak { coords: VariationCoords::new(axes.iter().copied()), ..Default::default() };
        fonts.font_data.insert(name.into(), Arc::new(FontData::from_owned(bytes).tweak(tweak)));
        true
    };
    // Bahnschrift это DIN 1451, шрифт немецких дорожных знаков. Он переменный, поэтому
    // одним файлом закрывает и текст, и узкие заголовки. Есть в Windows 10 и 11.
    let bahn = load("bahn", "bahnschrift.ttf", &[("wght", 400.0), ("wdth", 100.0)])
        && load("bahn-semi", "bahnschrift.ttf", &[("wght", 600.0), ("wdth", 100.0)])
        && load("bahn-display", "bahnschrift.ttf", &[("wght", 700.0), ("wdth", 75.0)]);
    let segoe = !bahn && load("bahn", "segoeui.ttf", &[]) && load("bahn-semi", "seguisb.ttf", &[]);
    let mono = load("mono", "CascadiaMono.ttf", &[("wght", 400.0)]) || load("mono", "consola.ttf", &[]);

    if bahn || segoe {
        fonts.families.entry(FontFamily::Proportional).or_default().insert(0, "bahn".into());
    }
    let chain = |first: &str| {
        let mut chain = vec![first.to_owned()];
        chain.extend(fallbacks.iter().cloned());
        chain
    };
    let semi = if bahn || segoe { chain("bahn-semi") } else { fallbacks.clone() };
    let big = if bahn { chain("bahn-display") } else { semi.clone() };
    fonts.families.insert(semibold(), semi);
    fonts.families.insert(display(), big);
    if mono {
        fonts.families.entry(FontFamily::Monospace).or_default().insert(0, "mono".into());
    }
    ctx.set_fonts(fonts);
}

fn visuals(p: &Palette, dark: bool) -> Visuals {
    let mut v = if dark { Visuals::dark() } else { Visuals::light() };
    let square = CornerRadius::ZERO;

    v.panel_fill = p.paper;
    v.window_fill = p.paper;
    v.window_stroke = Stroke::new(1.0, p.ink);
    v.window_corner_radius = square;
    v.menu_corner_radius = square;
    v.window_shadow = Shadow::NONE;
    v.popup_shadow = Shadow::NONE;
    v.faint_bg_color = p.paper_hover;
    v.extreme_bg_color = p.paper;
    v.text_edit_bg_color = Some(p.paper);
    v.weak_text_color = Some(p.ink_weak);
    v.hyperlink_color = p.ink;
    v.error_fg_color = p.error;
    v.warn_fg_color = p.ink;
    v.selection.bg_fill = p.ink.gamma_multiply(0.22);
    v.selection.stroke = Stroke::new(2.0, p.ink);
    v.striped = false;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.paper;
    w.noninteractive.weak_bg_fill = p.paper;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.rule);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.ink);
    w.noninteractive.corner_radius = square;

    w.inactive.bg_fill = p.paper;
    w.inactive.weak_bg_fill = p.paper;
    w.inactive.bg_stroke = Stroke::new(1.0, p.rule_strong);
    w.inactive.fg_stroke = Stroke::new(1.0, p.ink);
    w.inactive.corner_radius = square;

    w.hovered.bg_fill = p.paper_hover;
    w.hovered.weak_bg_fill = p.paper_hover;
    w.hovered.bg_stroke = Stroke::new(1.0, p.ink);
    w.hovered.fg_stroke = Stroke::new(1.0, p.ink);
    w.hovered.corner_radius = square;
    w.hovered.expansion = 0.0;

    w.active.bg_fill = p.rule;
    w.active.weak_bg_fill = p.rule;
    w.active.bg_stroke = Stroke::new(1.0, p.ink);
    w.active.fg_stroke = Stroke::new(1.0, p.ink);
    w.active.corner_radius = square;
    w.active.expansion = 0.0;

    w.open = w.active;
    v
}

/// Выключены ли анимации в настройках Windows.
#[cfg(windows)]
pub fn reduced_motion() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW};
    let mut enabled: i32 = 1;
    let ok = unsafe { SystemParametersInfoW(SPI_GETCLIENTAREAANIMATION, 0, (&raw mut enabled).cast(), 0) };
    ok != 0 && enabled == 0
}

#[cfg(not(windows))]
pub fn reduced_motion() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(c: Color32) -> f32 {
        let channel = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f32 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn text_and_lines_are_readable_in_both_themes() {
        for p in [&LIGHT, &DARK] {
            assert!(contrast(p.ink, p.paper) >= 7.0);
            assert!(contrast(p.ink_weak, p.paper) >= 4.5);
            assert!(contrast(p.ink_weak, p.paper_hover) >= 4.5);
            assert!(contrast(p.rule_strong, p.paper) >= 3.0);
            assert!(contrast(p.error, p.paper) >= 4.5);
            for line in p.lines {
                assert!(contrast(line, p.paper) >= 4.5, "{line:?} на бумаге");
                assert!(contrast(p.on_line, line) >= 4.5, "табличка {line:?}");
            }
        }
    }
}
