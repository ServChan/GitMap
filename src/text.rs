//! Текст: разбор и раскладка через Parley, глифы рисует Vello. Bahnschrift это DIN 1451,
//! шрифт немецких дорожных знаков: он переменный и закрывает одним файлом и текст, и узкие
//! заголовки. Готовые раскладки кэшируются: одни и те же подписи живут кадр за кадром.

use parley::{
    Alignment, AlignmentOptions, FontContext, FontFamily, FontWeight, FontWidth, Layout, LayoutContext,
    PositionedLayoutItem, StyleProperty,
};
use std::collections::HashMap;
use std::rc::Rc;
use vello::Scene;
use vello::kurbo::Affine;
use vello::peniko::{Color, Fill};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Face {
    Body,
    Semi,
    /// Узкое жирное начертание для крупных заголовков.
    Display,
    Mono,
}

pub type Text = Rc<Layout<()>>;

#[derive(PartialEq, Eq, Hash)]
struct Key {
    text: String,
    face: Face,
    size: u32,
    wrap: u32,
}

pub struct Fonts {
    fonts: FontContext,
    layouts: LayoutContext<()>,
    cache: HashMap<Key, Text>,
}

impl Fonts {
    pub fn new() -> Self {
        Self { fonts: FontContext::new(), layouts: LayoutContext::new(), cache: HashMap::new() }
    }

    pub fn layout(&mut self, text: &str, face: Face, size: f32, wrap: Option<f32>) -> Text {
        let key = Key {
            text: text.to_owned(),
            face,
            size: (size * 16.0) as u32,
            wrap: wrap.map_or(0, |w| (w * 2.0) as u32),
        };
        if let Some(layout) = self.cache.get(&key) {
            return layout.clone();
        }
        // Подписей на схеме мало, но имена меняются: кэш не должен расти бесконечно.
        if self.cache.len() > 6000 {
            self.cache.clear();
        }
        let mut builder = self.layouts.ranged_builder(&mut self.fonts, text, 1.0, true);
        builder.push_default(StyleProperty::FontSize(size));
        let family = match face {
            Face::Mono => "Cascadia Mono, Consolas, monospace",
            _ => "Bahnschrift, Segoe UI, sans-serif",
        };
        builder.push_default(StyleProperty::FontFamily(FontFamily::Source(family.into())));
        match face {
            Face::Body | Face::Mono => {}
            Face::Semi => builder.push_default(StyleProperty::FontWeight(FontWeight::new(600.0))),
            Face::Display => {
                builder.push_default(StyleProperty::FontWeight(FontWeight::new(700.0)));
                builder.push_default(StyleProperty::FontWidth(FontWidth::CONDENSED));
                builder.push_default(StyleProperty::LetterSpacing(size * -0.005));
            }
        }
        let mut layout: Layout<()> = builder.build(text);
        layout.break_all_lines(wrap);
        layout.align(Alignment::Start, AlignmentOptions::default());
        let layout = Rc::new(layout);
        self.cache.insert(key, layout.clone());
        layout
    }

    pub fn width(&mut self, text: &str, face: Face, size: f32) -> f32 {
        self.layout(text, face, size, None).width()
    }

    /// Строка, обрезанная многоточием до заданной ширины.
    pub fn fit(&mut self, text: &str, face: Face, size: f32, width: f32) -> Text {
        let full = self.layout(text, face, size, None);
        if full.width() <= width {
            return full;
        }
        let chars: Vec<char> = text.chars().collect();
        let (mut low, mut high) = (0, chars.len());
        while low < high {
            let middle = (low + high).div_ceil(2);
            let cut: String = chars[..middle].iter().collect();
            if self.width(&format!("{}…", cut.trim_end()), face, size) <= width {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
        let cut: String = chars[..low].iter().collect();
        self.layout(&format!("{}…", cut.trim_end()), face, size, None)
    }
}

/// Рисует раскладку. `transform` ставит левый верхний угол текста.
pub fn draw(scene: &mut Scene, text: &Layout<()>, transform: Affine, color: Color) {
    for line in text.lines() {
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(glyphs) = item else { continue };
            let run = glyphs.run();
            let synthesis = run.synthesis();
            let coords: Vec<i16> = run.normalized_coords().iter().map(|c| c.to_bits()).collect();
            let skew = synthesis.skew().map(|angle| Affine::skew(angle.to_radians().tan() as f64, 0.0));
            scene
                .draw_glyphs(run.font())
                .font_size(run.font_size())
                .transform(transform)
                .glyph_transform(skew)
                .normalized_coords(&coords)
                .brush(color)
                .hint(false)
                .draw(
                    Fill::NonZero,
                    glyphs.positioned_glyphs().map(|glyph| vello::Glyph { id: glyph.id, x: glyph.x, y: glyph.y }),
                );
        }
    }
}

/// Ореол цвета фона вокруг подписи: так делают на картах, чтобы текст читался поверх линий.
pub fn halo(scene: &mut Scene, text: &Layout<()>, transform: Affine, color: Color, width: f64) {
    let stroke = vello::kurbo::Stroke::new(width).with_join(vello::kurbo::Join::Round);
    for line in text.lines() {
        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(glyphs) = item else { continue };
            let run = glyphs.run();
            let coords: Vec<i16> = run.normalized_coords().iter().map(|c| c.to_bits()).collect();
            scene
                .draw_glyphs(run.font())
                .font_size(run.font_size())
                .transform(transform)
                .normalized_coords(&coords)
                .brush(color)
                .hint(false)
                .draw(&stroke, glyphs.positioned_glyphs().map(|glyph| vello::Glyph { id: glyph.id, x: glyph.x, y: glyph.y }));
        }
    }
}
