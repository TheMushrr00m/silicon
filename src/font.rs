//! A basic font manager with fallback support
//!
//! # Example
//!
//! ```rust
//! use image::{RgbImage, Rgb};
//! use silicon::font::{FontCollection, FontStyle};
//!
//! let mut image = RgbImage::new(250, 100);
//! let font = FontCollection::new(&[("Hack", 27.0), ("FiraCode", 27.0)]).unwrap();
//!
//! font.draw_text_mut(&mut image, Rgb([255, 0, 0]), 0, 0, FontStyle::REGULAR, "Hello, world");
//! ```
use crate::error::FontError;
use conv::ValueInto;
use euclid::default::{Point2D, Rect, Size2D};
use font_kit::canvas::{Canvas, Format, RasterizationOptions};
use font_kit::font::Font;
use font_kit::hinting::HintingOptions;
use font_kit::loader::FontTransform;
use font_kit::properties::{Properties, Style, Weight};
use font_kit::source::SystemSource;
use image::{GenericImage, Pixel};
use imageproc::definitions::Clamp;
use imageproc::pixelops::weighted_sum;
use std::collections::HashMap;
use std::sync::Arc;
use syntect::highlighting;

/// Font style
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum FontStyle {
    REGULAR,
    ITALIC,
    BOLD,
    BOLDITALIC,
}

impl From<highlighting::FontStyle> for FontStyle {
    fn from(style: highlighting::FontStyle) -> Self {
        if style.contains(highlighting::FontStyle::BOLD) {
            if style.contains(highlighting::FontStyle::ITALIC) {
                BOLDITALIC
            } else {
                BOLD
            }
        } else if style.contains(highlighting::FontStyle::ITALIC) {
            ITALIC
        } else {
            REGULAR
        }
    }
}

use FontStyle::*;

/// A single font with specific size
#[derive(Debug)]
pub struct ImageFont {
    pub fonts: HashMap<FontStyle, Font>,
    pub size: f32,
}

impl Default for ImageFont {
    /// It will use Hack font (size: 26.0) by default
    fn default() -> Self {
        let l = vec![
            (
                REGULAR,
                include_bytes!("../assets/fonts/Hack-Regular.ttf").to_vec(),
            ),
            (
                ITALIC,
                include_bytes!("../assets/fonts/Hack-Italic.ttf").to_vec(),
            ),
            (
                BOLD,
                include_bytes!("../assets/fonts/Hack-Bold.ttf").to_vec(),
            ),
            (
                BOLDITALIC,
                include_bytes!("../assets/fonts/Hack-BoldItalic.ttf").to_vec(),
            ),
        ];
        let mut fonts = HashMap::new();
        for (style, bytes) in l {
            let font = Font::from_bytes(Arc::new(bytes), 0).unwrap();
            fonts.insert(style, font);
        }

        Self { fonts, size: 26.0 }
    }
}

impl ImageFont {
    pub fn new(name: &str, size: f32) -> Result<Self, FontError> {
        // Silicon already contains Hack font
        if name == "Hack" {
            let mut font = Self::default();
            font.size = size;
            return Ok(font);
        }

        let mut fonts = HashMap::new();

        let family = SystemSource::new().select_family_by_name(name)?;
        let handles = family.fonts();

        debug!("{:?}", handles);

        for handle in handles {
            let font = handle.load()?;

            let properties: Properties = font.properties();

            debug!("{:?} - {:?}", font, properties);

            // cannot use match because `Weight` didn't derive `Eq`
            match properties.style {
                Style::Normal => {
                    if properties.weight == Weight::NORMAL {
                        fonts.insert(REGULAR, font);
                    } else if properties.weight == Weight::BOLD {
                        fonts.insert(BOLD, font);
                    }
                }
                Style::Italic => {
                    if properties.weight == Weight::NORMAL {
                        fonts.insert(ITALIC, font);
                    } else if properties.weight == Weight::BOLD {
                        fonts.insert(BOLDITALIC, font);
                    }
                }
                _ => (),
            }
        }

        Ok(Self { fonts, size })
    }

    /// Get a font by style. If there is no such a font, it will return the REGULAR font.
    pub fn get_by_style(&self, style: FontStyle) -> &Font {
        self.fonts
            .get(&style)
            .unwrap_or_else(|| self.fonts.get(&REGULAR).unwrap())
    }

    /// Get the regular font
    pub fn get_regular(&self) -> &Font {
        self.fonts.get(&REGULAR).unwrap()
    }

    /// Get the height of the font
    pub fn get_font_height(&self) -> u32 {
        let font = self.get_regular();
        let metrics = font.metrics();
        ((metrics.ascent - metrics.descent) / metrics.units_per_em as f32 * self.size).ceil() as u32
    }
}

/// A collection of font
///
/// It can be used to draw text on the image.
#[derive(Debug)]
pub struct FontCollection(Vec<ImageFont>);

impl Default for FontCollection {
    fn default() -> Self {
        Self(vec![ImageFont::default()])
    }
}

impl FontCollection {
    /// Create a FontCollection with several fonts.
    pub fn new<S: AsRef<str>>(font_list: &[(S, f32)]) -> Result<Self, FontError> {
        let mut fonts = vec![];
        for (name, size) in font_list {
            let name = name.as_ref();
            match ImageFont::new(name, *size) {
                Ok(font) => fonts.push(font),
                Err(err) => eprintln!("[error] Error occurs when load font `{}`: {}", name, err),
            }
        }
        Ok(Self(fonts))
    }

    fn glyph_for_char(&self, c: char, style: FontStyle) -> Option<(u32, &ImageFont, &Font)> {
        for font in &self.0 {
            let result = font.get_by_style(style);
            if let Some(id) = result.glyph_for_char(c) {
                return Some((id, font, result));
            }
        }
        eprintln!("[warning] No font found for character `{}`", c);
        None
    }

    /// get max height of all the fonts
    pub fn get_font_height(&self) -> u32 {
        self.0
            .iter()
            .map(|font| font.get_font_height())
            .max()
            .unwrap()
    }

    fn layout(&self, text: &str, style: FontStyle) -> (Vec<PositionedGlyph>, u32) {
        let mut delta_x = 0;
        let height = self.get_font_height();

        let glyphs = text
            .chars()
            .filter_map(|c| {
                self.glyph_for_char(c, style).map(|(id, imfont, font)| {
                    let raster_rect = font
                        .raster_bounds(
                            id,
                            imfont.size,
                            &FontTransform::identity(),
                            &Point2D::zero(),
                            HintingOptions::None,
                            RasterizationOptions::GrayscaleAa,
                        )
                        .unwrap();
                    let x = delta_x as i32 + raster_rect.origin.x;
                    let y = height as i32 - raster_rect.size.height - raster_rect.origin.y;
                    delta_x += Self::get_glyph_width(font, id, imfont.size);

                    PositionedGlyph {
                        id,
                        font: font.clone(),
                        size: imfont.size,
                        raster_rect,
                        position: Point2D::new(x, y),
                    }
                })
            })
            .collect();

        (glyphs, delta_x)
    }

    /// Get the width of the given glyph
    fn get_glyph_width(font: &Font, id: u32, size: f32) -> u32 {
        let metrics = font.metrics();
        let advance = font.advance(id).unwrap();
        (advance / metrics.units_per_em as f32 * size).x.ceil() as u32
    }

    /// Get the width of the given text
    pub fn get_text_len(&self, text: &str) -> u32 {
        self.layout(text, REGULAR).1
    }

    /// Draw the text to a image
    /// return the width of written text
    pub fn draw_text_mut<I>(
        &self,
        image: &mut I,
        color: I::Pixel,
        x: u32,
        y: u32,
        style: FontStyle,
        text: &str,
    ) -> u32
    where
        I: GenericImage,
        <I::Pixel as Pixel>::Subpixel: ValueInto<f32> + Clamp<f32>,
    {
        let metrics = self.0[0].get_regular().metrics();
        let offset =
            (metrics.descent / metrics.units_per_em as f32 * self.0[0].size).round() as i32;

        let (glyphs, width) = self.layout(text, style);

        for glyph in glyphs {
            glyph.draw(offset, |px, py, v| {
                if v <= std::f32::EPSILON {
                    return;
                }
                let (x, y) = ((px + x as i32) as u32, (py + y as i32) as u32);
                let pixel = image.get_pixel(x, y);
                let weighted_color = weighted_sum(pixel, color, 1.0 - v, v);
                image.put_pixel(x, y, weighted_color);
            })
        }

        width
    }
}

struct PositionedGlyph {
    id: u32,
    font: Font,
    size: f32,
    position: Point2D<i32>,
    raster_rect: Rect<i32>,
}

impl PositionedGlyph {
    fn draw<O: FnMut(i32, i32, f32)>(&self, offset: i32, mut o: O) {
        let mut canvas = Canvas::new(&self.raster_rect.size.to_u32(), Format::A8);

        let origin = Point2D::new(
            -self.raster_rect.origin.x,
            self.raster_rect.size.height + self.raster_rect.origin.y,
        )
        .to_f32();

        // don't rasterize whitespace(https://github.com/pcwalton/font-kit/issues/7)
        // TODO: width of TAB ?
        if canvas.size != Size2D::new(0, 0) {
            self.font
                .rasterize_glyph(
                    &mut canvas,
                    self.id,
                    self.size,
                    &FontTransform::identity(),
                    &origin,
                    HintingOptions::None,
                    RasterizationOptions::GrayscaleAa,
                )
                .unwrap();
        }

        for y in (0..self.raster_rect.size.height).rev() {
            let (row_start, row_end) =
                (y as usize * canvas.stride, (y + 1) as usize * canvas.stride);
            let row = &canvas.pixels[row_start..row_end];

            for x in 0..self.raster_rect.size.width {
                let val = f32::from(row[x as usize]) / 255.0;
                let px = self.position.x + x;
                let py = self.position.y + y + offset;

                o(px, py, val);
            }
        }
    }
}
