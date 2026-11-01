//! Оформление: две темы одной схемы. Днём печать на тёплой бумаге, ночью светящееся
//! табло на тёмном стекле. Цвет несут только ветки, всё остальное держат нейтрали.
//! Контраст проверяется тестами внизу файла.

use crate::layout::PALETTE_SIZE;
use vello::peniko::Color;

pub struct Palette {
    /// Фон схемы.
    pub ground: Color,
    /// Лист панели поверх схемы.
    pub sheet: Color,
    pub sheet_hover: Color,
    /// Основная линия, текст, выбранная строка.
    pub ink: Color,
    /// Вторичный текст (не ниже 4.5:1 на листе и на фоне).
    pub ink_weak: Color,
    /// Декоративные линии.
    pub rule: Color,
    /// Границы элементов управления (не ниже 3:1).
    pub rule_strong: Color,
    /// Цвета веток (не ниже 4.5:1 на фоне: ими же подписаны линии).
    pub lines: [Color; PALETTE_SIZE],
    /// Текст на табличке цвета ветки.
    pub on_line: Color,
    pub danger: Color,
    pub on_danger: Color,
    pub shadow: Color,
}

const fn rgb(value: u32) -> Color {
    Color::from_rgb8((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

pub const LIGHT: Palette = Palette {
    ground: rgb(0xEFEADD),
    sheet: rgb(0xFAF7EF),
    sheet_hover: rgb(0xEEE9DB),
    ink: rgb(0x1B1D21),
    ink_weak: rgb(0x5B5A54),
    rule: rgb(0xD8D2C1),
    rule_strong: rgb(0x87826F),
    lines: [
        rgb(0xC8102E),
        rgb(0x0B5CAD),
        rgb(0x12743A),
        rgb(0x6B3FA0),
        rgb(0xA94B00),
        rgb(0x006F70),
        rgb(0xA8146F),
        rgb(0x774719),
        rgb(0x5C6600),
        rgb(0x3A567C),
    ],
    on_line: rgb(0xFBF8F0),
    danger: rgb(0xA6171F),
    on_danger: rgb(0xFBF8F0),
    shadow: Color::from_rgba8(58, 46, 22, 70),
};

pub const DARK: Palette = Palette {
    ground: rgb(0x101317),
    sheet: rgb(0x191D23),
    sheet_hover: rgb(0x242A32),
    ink: rgb(0xEDE7D9),
    ink_weak: rgb(0xA9A598),
    rule: rgb(0x2C3239),
    rule_strong: rgb(0x7D8086),
    lines: [
        rgb(0xFF6F61),
        rgb(0x62AEFF),
        rgb(0x4FCC7D),
        rgb(0xBB9DFF),
        rgb(0xFFA552),
        rgb(0x40D0C9),
        rgb(0xFF7FC8),
        rgb(0xD6A873),
        rgb(0xC6D052),
        rgb(0xA3B9D9),
    ],
    on_line: rgb(0x12151A),
    danger: rgb(0xFF8F88),
    on_danger: rgb(0x12151A),
    shadow: Color::from_rgba8(0, 0, 0, 150),
};

pub fn mix(a: Color, b: Color, t: f32) -> Color {
    let (x, y) = (a.components, b.components);
    let m = |i: usize| x[i] + (y[i] - x[i]) * t;
    Color::new([m(0), m(1), m(2), m(3)])
}

pub fn fade(color: Color, alpha: f32) -> Color {
    color.multiply_alpha(alpha)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn luminance(c: Color) -> f32 {
        let channel = |v: f32| if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
        0.2126 * channel(c.components[0]) + 0.7152 * channel(c.components[1]) + 0.0722 * channel(c.components[2])
    }

    fn contrast(a: Color, b: Color) -> f32 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn text_and_lines_are_readable_in_both_themes() {
        for p in [&LIGHT, &DARK] {
            for surface in [p.ground, p.sheet] {
                assert!(contrast(p.ink, surface) >= 7.0);
                assert!(contrast(p.ink_weak, surface) >= 4.5);
                assert!(contrast(p.rule_strong, surface) >= 3.0);
                assert!(contrast(p.danger, surface) >= 4.5);
            }
            assert!(contrast(p.ink_weak, p.sheet_hover) >= 4.5);
            assert!(contrast(p.on_danger, p.danger) >= 4.5);
            for line in p.lines {
                assert!(contrast(line, p.ground) >= 4.5, "{line:?} на фоне");
                assert!(contrast(p.on_line, line) >= 4.5, "табличка {line:?}");
            }
        }
    }
}
