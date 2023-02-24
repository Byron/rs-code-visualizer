use crate::render::{BgColor, FgColor};
use bstr::ByteSlice;
use image::{ImageBuffer, Rgb};
use std::ops::{Deref, DerefMut};
use syntect::highlighting::{Color, Style};
use unifont_bitmap::{Bitmap, Unifont};

/// The result of processing a chunk.
pub struct Outcome {
    /// The longest line we encountered in unicode codepoints.
    pub longest_line_in_chars: usize,
    /// The last used background color
    pub background: Option<Rgb<u8>>,
}

pub struct Context {
    pub column_width: u32,
    pub line_height: u32,
    pub char_width: u32,
    pub total_line_count: u32,
    pub line_num: u32,
    pub lines_per_column: u32,

    pub fg_color: FgColor,
    pub bg_color: BgColor,
    pub highlight_truncated_lines: bool,

    pub file_index: usize,
    pub color_modulation: f32,
    pub tab_spaces: u32,
}

/// Return the `(x, y)` offsets to apply to the given line, to wrap columns of lines into the
/// target image.
pub fn calc_offsets(
    line_num: u32,
    lines_per_column: u32,
    column_width: u32,
    line_height: u32,
) -> (u32, u32) {
    (
        (line_num / lines_per_column) * column_width,
        (line_num % lines_per_column) * line_height,
    )
}

pub fn process<C>(
    content: &str,
    img: &mut ImageBuffer<Rgb<u8>, C>,
    mut highlight: impl FnMut(&str) -> Result<Vec<(Style, &str)>, syntect::Error>,
    Context {
        column_width,
        line_height,
        char_width,
        total_line_count,
        highlight_truncated_lines,
        mut line_num,
        lines_per_column,
        fg_color,
        bg_color,
        file_index,
        color_modulation,
        tab_spaces,
    }: Context,
) -> anyhow::Result<Outcome>
where
    C: Deref<Target = [u8]>,
    C: DerefMut,
{
    let mut unifont = Unifont::open();

    let mut longest_line_in_chars = 0;
    let mut background = None::<Rgb<u8>>;
    for line in content.as_bytes().lines_with_terminator() {
        let (line, truncated_line) = {
            let line = line.to_str().expect("UTF-8 was source");
            let mut num_chars = 0;
            let mut chars = line.chars();
            let bytes_till_char_limit: usize = chars
                .by_ref()
                .take(column_width as usize)
                .map(|c| {
                    num_chars += 1;
                    c.len_utf8()
                })
                .sum();
            num_chars += chars.count();
            longest_line_in_chars = longest_line_in_chars.max(num_chars);
            let possibly_truncated_line = (num_chars >= column_width as usize)
                .then(|| &line[..bytes_till_char_limit])
                .unwrap_or(line);
            (
                if highlight_truncated_lines {
                    possibly_truncated_line
                } else {
                    line
                },
                possibly_truncated_line,
            )
        };

        // println!("line: {:?}", line);

        let actual_line = line_num % total_line_count;
        let (cur_column_x_offset, cur_y) = calc_offsets(
            actual_line,
            lines_per_column,
            column_width * char_width,
            line_height,
        );
        let storage;
        let array_storage;

        let regions: &[_] = if line.len() > 1024 * 16 {
            array_storage = [(default_bg_color(background), truncated_line)];
            &array_storage
        } else {
            storage = highlight(line)?;
            &storage
        };
        let background = background
            .get_or_insert_with(|| bg_color.to_rgb(regions[0].0, file_index, color_modulation));
        let mut cur_line_x = 0;

        for (style, region) in regions {
            if cur_line_x >= column_width {
                break;
            }
            if region.is_empty() {
                continue;
            }

            for chr in region.chars() {
                if cur_line_x >= column_width {
                    break;
                }

                let char_color: Rgb<u8> = match fg_color {
                    FgColor::Style => {
                        Rgb([style.foreground.r, style.foreground.g, style.foreground.b])
                    }
                    FgColor::StyleAsciiBrightness => {
                        let fg_byte = (chr as usize) & 0xff;
                        let boost = 2.4;
                        Rgb([
                            (((fg_byte * style.foreground.r as usize) as f32 / u16::MAX as f32)
                                * boost
                                * 256.0) as u8,
                            (((fg_byte * style.foreground.g as usize) as f32 / u16::MAX as f32)
                                * boost
                                * 256.0) as u8,
                            (((fg_byte * style.foreground.b as usize) as f32 / u16::MAX as f32)
                                * boost
                                * 256.0) as u8,
                        ])
                    }
                };

                if chr == ' ' || chr == '\n' || chr == '\r' {
                    // for y_pos in cur_y..cur_y + line_height {
                    // img.put_pixel(
                    //     cur_column_x_offset + cur_line_x * char_width,
                    //     y_pos,
                    //     *background,
                    // );
                    put_char_in_image(
                        chr,
                        &mut unifont,
                        cur_column_x_offset + cur_line_x * char_width,
                        cur_y,
                        img,
                        background,
                        &char_color,
                        &mut cur_line_x,
                    );
                    // }
                } else if chr == '\t' {
                    let spaces_to_add = tab_spaces - (cur_line_x % tab_spaces);

                    for _ in 0..spaces_to_add {
                        if cur_line_x >= column_width {
                            break;
                        }

                        // for y_pos in cur_y..cur_y + line_height {
                        // img.put_pixel(
                        //     cur_column_x_offset + cur_line_x * char_width,
                        //     y_pos,
                        //     *background,
                        // );
                        put_char_in_image(
                            chr,
                            &mut unifont,
                            cur_column_x_offset + cur_line_x * char_width,
                            cur_y,
                            img,
                            background,
                            &char_color,
                            &mut cur_line_x,
                        );
                        // }
                    }
                } else {
                    // for y_pos in cur_y..cur_y + line_height {
                    // img.put_pixel(
                    //     cur_column_x_offset + cur_line_x * char_width,
                    //     y_pos,
                    //     char_color,
                    // );
                    put_char_in_image(
                        chr,
                        &mut unifont,
                        cur_column_x_offset + cur_line_x * char_width,
                        cur_y,
                        img,
                        background,
                        &char_color,
                        &mut cur_line_x,
                    );
                    // }
                }
            }
        }

        while cur_line_x < column_width {
            // for y_pos in cur_y..cur_y + line_height {
            // img.put_pixel(
            //     cur_column_x_offset + cur_line_x * char_width,
            //     y_pos,
            //     *background,
            // );
            put_char_in_image(
                ' ',
                &mut unifont,
                cur_column_x_offset + cur_line_x * char_width,
                cur_y,
                img,
                background,
                background,
                &mut cur_line_x,
            );
            // }
        }

        line_num += 1;
    }

    Ok(Outcome {
        longest_line_in_chars,
        background,
    })
}

fn put_char_in_image<C>(
    chr: char,
    unifont: &mut Unifont,
    img_x: u32,
    img_y: u32,
    img: &mut ImageBuffer<Rgb<u8>, C>,
    background_color: &Rgb<u8>,
    text_color: &Rgb<u8>,
    cur_line_x: &mut u32,
) where
    C: Deref<Target = [u8]>,
    C: DerefMut,
{
    let bitmap = unifont.load_bitmap(chr.into());

    // get bitmap dimensions
    let char_height = 16;
    // let standard_char_width = 8;
    let char_width = if bitmap.is_wide() { 16 } else { 8 };

    // add bitmap to image
    for y in 0..char_height as usize {
        for x in 0..char_width {
            let pixel_x = img_x + x;
            let pixel_y = img_y + y as u32;

            // get pixel from bitmap
            let should_pixel = if bitmap.is_wide() {
                bitmap.get_bytes()[y * 2 + x as usize / 8] & (1 << (7 - x % 8)) != 0
            } else {
                bitmap.get_bytes()[y] & (1 << (7 - x)) != 0
            };

            // if not in image bounds
            if pixel_x >= img.width() || pixel_y >= img.height() {
                println!(
                    "Spipping pixel. out of bounds: {}, {}",
                    img_x + x,
                    img_y + y as u32
                );
                continue;
            } else {
                // set pixel in image
                if should_pixel {
                    img.put_pixel(pixel_x, pixel_y, *text_color);
                } else {
                    img.put_pixel(pixel_x, pixel_y, *background_color);
                }
            }
        }
    }

    if bitmap.is_wide() {
        *cur_line_x += 2;
    } else {
        *cur_line_x += 1;
    }
}

fn default_bg_color(background: Option<Rgb<u8>>) -> Style {
    Style {
        foreground: Color {
            r: 200,
            g: 200,
            b: 200,
            a: u8::MAX,
        },
        background: background
            .map(|c| Color {
                r: c.0[0],
                g: c.0[1],
                b: c.0[2],
                a: u8::MAX,
            })
            .unwrap_or(Color::BLACK),
        font_style: Default::default(),
    }
}
