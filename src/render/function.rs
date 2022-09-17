use crate::render::chunk::calc_offsets;
use crate::render::{chunk, Options};
use anyhow::{bail, Context};
use image::{ImageBuffer, Pixel, Rgb, RgbImage};
use memmap2::MmapMut;
use prodash::Progress;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

/// determine number and height of columns closest to desired aspect ratio
fn determine_dimensions(
    target_aspect_ratio: f64,
    column_width: u32,
    total_line_count: u32,
    line_height: u32,
    force_full_columns: bool,
    mut progress: impl prodash::Progress,
) -> anyhow::Result<(ImageBuffer<Rgb<u8>, MmapMut>, u32, u32)> {
    // determine image dimensions based on num of lines and constraints
    let mut lines_per_column = 1;
    let mut last_checked_aspect_ratio: f64 = f64::MAX;
    let mut last_column_line_limit = lines_per_column;
    let mut required_columns;
    let mut cur_aspect_ratio: f64 =
        column_width as f64 * total_line_count as f64 / (lines_per_column as f64 * 2.0);

    // determine maximum aspect ratios
    let tallest_aspect_ratio = column_width as f64 / total_line_count as f64 * 2.0;
    let widest_aspect_ratio = total_line_count as f64 * column_width as f64 / 2.0;

    if target_aspect_ratio <= tallest_aspect_ratio {
        // use tallest possible aspect ratio
        lines_per_column = total_line_count;
        required_columns = 1;
    } else if target_aspect_ratio >= widest_aspect_ratio {
        // use widest possible aspect ratio
        lines_per_column = 1;
        required_columns = total_line_count;
    } else {
        // start at widest possible aspect ratio
        lines_per_column = 1;
        // required_columns = line_count;

        // de-widen aspect ratio until closest match is found
        while (last_checked_aspect_ratio - target_aspect_ratio).abs()
            > (cur_aspect_ratio - target_aspect_ratio).abs()
        {
            // remember current aspect ratio
            last_checked_aspect_ratio = cur_aspect_ratio;

            if force_full_columns {
                last_column_line_limit = lines_per_column;

                // determine required number of columns
                required_columns = total_line_count / lines_per_column;
                if total_line_count % lines_per_column != 0 {
                    required_columns += 1;
                }

                let last_required_columns = required_columns;

                // find next full column aspect ratio
                while required_columns == last_required_columns {
                    lines_per_column += 1;

                    // determine required number of columns
                    required_columns = total_line_count / lines_per_column;
                    if total_line_count % lines_per_column != 0 {
                        required_columns += 1;
                    }
                }
            } else {
                // generate new aspect ratio

                lines_per_column += 1;

                // determine required number of columns
                required_columns = total_line_count / lines_per_column;
                if total_line_count % lines_per_column != 0 {
                    required_columns += 1;
                }
            }

            cur_aspect_ratio = required_columns as f64 * column_width as f64
                / (lines_per_column as f64 * line_height as f64);
        }

        //> re-determine best aspect ratio

        // (Should never not happen, but)
        // previous while loop would never have been entered if (column_line_limit == 1)
        // so (column_line_limit -= 1;) would be unnecessary
        if lines_per_column != 1 && !force_full_columns {
            // revert to last aspect ratio
            lines_per_column -= 1;
        } else if force_full_columns {
            lines_per_column = last_column_line_limit;
        }

        // determine required number of columns
        required_columns = total_line_count / lines_per_column;
        if total_line_count % lines_per_column != 0 {
            required_columns += 1;
        }
    }

    let imgx: u32 = required_columns * column_width;
    let imgy: u32 = total_line_count.min(lines_per_column) * line_height;
    let channel_count = Rgb::<u8>::CHANNEL_COUNT;
    let num_pixels = imgx as usize * imgy as usize * channel_count as usize;
    progress.info(format!(
        "Image dimensions: {imgx} x {imgy} x {channel_count} ({} in virtual memory)",
        bytesize::ByteSize(num_pixels as u64)
    ));

    let img =
        ImageBuffer::<Rgb<u8>, _>::from_raw(imgx, imgy, memmap2::MmapMut::map_anon(num_pixels)?)
            .expect("correct size computation above");

    progress.info(format!(
        "Aspect ratio is {} off from target",
        (last_checked_aspect_ratio - target_aspect_ratio).abs(),
    ));
    Ok((img, lines_per_column, required_columns))
}

pub fn render(
    content: Vec<(PathBuf, String)>,
    mut progress: impl prodash::Progress,
    should_interrupt: &AtomicBool,
    Options {
        column_width,
        line_height,
        target_aspect_ratio,
        threads,
        fg_color,
        bg_color,
        highlight_truncated_lines,
        display_to_be_processed_file,
        themes,
        force_full_columns,
        plain,
        ignore_files_without_syntax,
        color_modulation,
    }: Options,
) -> anyhow::Result<ImageBuffer<Rgb<u8>, MmapMut>> {
    // unused for now
    // could be used to make a "rolling code" animation
    let start = std::time::Instant::now();

    let ss = SyntaxSet::load_defaults_newlines();

    //> read files (for /n counting)
    let (content, total_line_count, num_ignored) = {
        let mut out = Vec::with_capacity(content.len());
        let mut lines = 0;
        let mut num_ignored = 0;
        for (path, content) in content {
            let num_content_lines = content.lines().count();
            lines += num_content_lines;
            if ignore_files_without_syntax && ss.find_syntax_for_file(&path)?.is_none() {
                lines -= num_content_lines;
                num_ignored += 1;
            } else {
                out.push(((path, content), num_content_lines))
            }
        }
        (out, lines as u32, num_ignored)
    };

    if total_line_count == 0 {
        bail!(
            "Did not find a single line to render in {} files",
            content.len()
        );
    }

    // determine number and height of columns closest to desired aspect ratio
    let (mut img, lines_per_column, required_columns) = determine_dimensions(
        target_aspect_ratio,
        column_width,
        total_line_count,
        line_height,
        force_full_columns,
        progress.add_child("determine dimensions"),
    )?;

    progress.set_name("process");
    progress.init(
        Some(content.len()),
        prodash::unit::label_and_mode("files", prodash::unit::display::Mode::with_percentage())
            .into(),
    );
    let mut line_progress = progress.add_child("render");
    line_progress.init(
        Some(total_line_count as usize),
        prodash::unit::label_and_mode("lines", prodash::unit::display::Mode::with_throughput())
            .into(),
    );

    let ts = ThemeSet::load_defaults();
    let mut prev_syntax = ss.find_syntax_plain_text() as *const _;
    let themes: Vec<_> = if themes.is_empty() {
        vec![ts.themes.get("Solarized (dark)").expect("built-in")]
    } else {
        themes
            .iter()
            .map(|theme| {
                ts.themes.get(theme).with_context(|| {
                    format!(
                        "Could not find theme {theme:?}, must be one of {}",
                        ts.themes
                            .keys()
                            .map(|s| format!("{s:?}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })
            })
            .collect::<Result<_, _>>()?
    };
    let theme = &themes[0]; // TODO: figure out what state is per theme actually.

    let threads = (threads == 0)
        .then(num_cpus::get)
        .unwrap_or(threads)
        .clamp(1, num_cpus::get());
    let (mut line_num, longest_line_chars, background) = if threads < 2 {
        let mut line_num: u32 = 0;
        let mut longest_line_chars = 0;
        let mut background = None;
        let mut highlighter =
            syntect::easy::HighlightLines::new(ss.find_syntax_plain_text(), theme);
        for (file_index, ((path, content), num_content_lines)) in content.into_iter().enumerate() {
            progress.inc();
            if should_interrupt.load(Ordering::Relaxed) {
                bail!("Cancelled by user")
            }

            if !plain {
                let syntax = ss
                    .find_syntax_for_file(&path)?
                    .unwrap_or_else(|| ss.find_syntax_plain_text());
                if syntax as *const _ != prev_syntax {
                    highlighter = syntect::easy::HighlightLines::new(syntax, theme);
                    prev_syntax = syntax as *const _;
                }
            }

            if display_to_be_processed_file {
                progress.info(format!("{path:?}"))
            }
            let out = chunk::process(
                &content,
                &mut img,
                |line| highlighter.highlight_line(line, &ss),
                chunk::Context {
                    column_width,
                    line_height,
                    total_line_count,
                    highlight_truncated_lines,
                    line_num,
                    lines_per_column,
                    fg_color,
                    bg_color,
                    file_index,
                    color_modulation,
                },
            )?;
            longest_line_chars = out.longest_line_in_chars.max(longest_line_chars);
            line_num += num_content_lines as u32;
            line_progress.inc_by(num_content_lines);
            background = out.background;
        }

        (line_num, longest_line_chars, background)
    } else {
        // multi-threaded rendering overview:
        //
        // Spawns threadpool and each file to be renered is sent to a thread as a message via a flume channel.
        // Upon recieving a message, a thread renders the entire file to an image of one column width.
        // and then returns that image to this main thread via a flume channel, to be stitched together
        // into one large image. The ordering of files rendered in the final image is remembered and
        // independant of thread rendering order.

        let mut line_num: u32 = 0;
        let mut longest_line_chars = 0;
        let mut background = None;
        std::thread::scope(|scope| -> anyhow::Result<()> {
            let (tx, rx) = flume::bounded::<(_, String, _, _, _)>(content.len());
            let (ttx, trx) = flume::unbounded();
            for tid in 0..threads {
                scope.spawn({
                    let rx = rx.clone();
                    let ttx = ttx.clone();
                    let ss = &ss;
                    let mut progress = line_progress.add_child(format!("Thread {tid}"));
                    move || -> anyhow::Result<()> {
                        let mut prev_syntax = ss.find_syntax_plain_text() as *const _;
                        let mut highlighter =
                            syntect::easy::HighlightLines::new(ss.find_syntax_plain_text(), theme);
                        for (path, content, num_content_lines, lines_so_far, file_index) in rx {
                            if !plain {
                                let syntax = ss
                                    .find_syntax_for_file(&path)?
                                    .unwrap_or_else(|| ss.find_syntax_plain_text());
                                if syntax as *const _ != prev_syntax {
                                    highlighter = syntect::easy::HighlightLines::new(syntax, theme);
                                    prev_syntax = syntax as *const _;
                                }
                            }

                            // create an image that fits one column
                            let mut img =
                                RgbImage::new(column_width, num_content_lines as u32 * line_height);

                            if display_to_be_processed_file {
                                progress.info(format!("{path:?}"))
                            }
                            let out = chunk::process(
                                &content,
                                &mut img,
                                |line| highlighter.highlight_line(line, ss),
                                chunk::Context {
                                    column_width,
                                    line_height,
                                    total_line_count,
                                    highlight_truncated_lines,
                                    line_num: 0,
                                    lines_per_column: total_line_count,
                                    fg_color,
                                    bg_color,
                                    file_index,
                                    color_modulation,
                                },
                            )?;
                            ttx.send((img, out, num_content_lines, lines_so_far))?;
                        }
                        Ok(())
                    }
                });
            }
            drop((rx, ttx));
            let mut lines_so_far = 0u32;
            for (file_index, ((path, content), num_content_lines)) in
                content.into_iter().enumerate()
            {
                tx.send((path, content, num_content_lines, lines_so_far, file_index))?;
                lines_so_far += num_content_lines as u32;
            }
            drop(tx);

            // for each file image that was rendered by a thread.
            for (sub_img, out, num_content_lines, lines_so_far) in trx {
                longest_line_chars = out.longest_line_in_chars.max(longest_line_chars);
                background = out.background;

                let calc_offsets = |line_num: u32| {
                    let actual_line = line_num % total_line_count;
                    calc_offsets(actual_line, lines_per_column, column_width, line_height)
                };

                // transfer pixels from sub_img to img. Where sub_img is a 1 column wide
                // image of one file. And img is our multi-column wide final output image.
                for line in 0..num_content_lines as u32 {
                    let (x_offset, line_y) = calc_offsets(lines_so_far + line);
                    for x in 0..column_width {
                        for height in 0..line_height {
                            let pix = sub_img.get_pixel(x, line * line_height + height);
                            img.put_pixel(x_offset + x, line_y + height, *pix);
                        }
                    }
                }

                line_progress.inc_by(num_content_lines);
                line_num += num_content_lines as u32;
                progress.inc();
                if should_interrupt.load(Ordering::Relaxed) {
                    bail!("Cancelled by user")
                }
            }
            Ok(())
        })?;
        (line_num, longest_line_chars, background)
    };

    // fill in any empty bottom right corner, with background color
    while line_num < lines_per_column * required_columns {
        let (cur_column_x_offset, cur_y) =
            calc_offsets(line_num, lines_per_column, column_width, line_height);
        let background = background.unwrap_or(Rgb([0, 0, 0]));

        for cur_line_x in 0..column_width {
            for y_pos in cur_y..cur_y + line_height {
                img.put_pixel(cur_column_x_offset + cur_line_x, y_pos, background);
            }
        }
        line_num += 1;
    }

    progress.show_throughput(start);
    line_progress.show_throughput(start);
    progress.info(format!(
        "Longest encountered line in chars: {longest_line_chars}"
    ));
    if num_ignored != 0 {
        progress.info(format!("Ignored {num_ignored} files due to missing syntax",))
    }

    Ok(img)
}
