//! `folio --render out.png [--width W] [--height H] [--scroll Y] [--zoom Z] [--dark] FILE`
//! `folio --bench FILE`
//!
//! Renders without a display (Cairo instead of GTK) for tests, screenshots
//! and performance measurement.

use crate::doc;
use crate::layout::{Engine, Img};
use crate::paint::CairoPainter;
use crate::theme::Theme;
use gtk::gdk;
use gtk::prelude::*;
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

pub fn run(args: &[String]) -> ExitCode {
    let mut out = None;
    let (mut w, mut h, mut scroll, mut zoom) = (1000.0, 1300.0, 0.0, 1.0);
    let (mut dark, mut bench, mut select, mut file) = (false, false, false, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut num = || it.next().and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
        match a.as_str() {
            "--render" => out = it.next().cloned(),
            "--width" => w = num(),
            "--height" => h = num(),
            "--scroll" => scroll = num(),
            "--zoom" => zoom = num(),
            "--dark" => dark = true,
            "--bench" => bench = true,
            "--select-all" => select = true,
            f => file = Some(f.to_string()),
        }
    }
    let Some(file) = file else {
        eprintln!("usage: folio --render OUT.png [--width W] [--height H] [--scroll Y] [--dark] FILE");
        return ExitCode::FAILURE;
    };
    let path = Path::new(&file);
    let t0 = Instant::now();
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{file}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let t_read = t0.elapsed();
    let d = doc::parse(&src);
    let t_parse = t0.elapsed();
    let nblocks = d.blocks.len();

    let fm = pangocairo::FontMap::new();
    let ctx = fm.create_context();
    let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let theme = if dark { Theme::dark() } else { Theme::light() };
    let mut e = Engine::new(ctx, d, base, theme, zoom);
    e.set_width(w);
    let t_engine = t0.elapsed();

    if bench {
        e.ensure(0.0, h);
        let t_first = t0.elapsed();
        // Same amount of layout again, now that fonts are loaded and cached.
        let t = Instant::now();
        e.ensure(h * 6.0, h * 7.0);
        let t_warm = t.elapsed();
        let t_all0 = t0.elapsed();
        while !e.measure_some(Duration::from_secs(10)) {}
        let t_all = t0.elapsed();
        let total = e.total_height();
        // Simulated scroll through the whole document, one screen at a time.
        let mut frames = Vec::new();
        let mut y = 0.0;
        let surf = cairo::ImageSurface::create(cairo::Format::ARgb32, w as i32, h as i32).unwrap();
        let cr = cairo::Context::new(&surf).unwrap();
        while y < total {
            let t = Instant::now();
            e.ensure(y, y + h);
            frames.push(t.elapsed());
            y += h * 0.5;
        }
        let t = Instant::now();
        e.paint(&mut CairoPainter(&cr), 0.0, h);
        let t_paint = t.elapsed();
        let t = Instant::now();
        let n = e.find("the");
        let t_find = t.elapsed();
        frames.sort();
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        println!("file: {} bytes, {} blocks, {:.0}px tall", src.len(), nblocks, total);
        println!("read          {:7.2} ms", ms(t_read));
        println!("parse         {:7.2} ms", ms(t_parse - t_read));
        println!("engine init   {:7.2} ms", ms(t_engine - t_parse));
        println!("first screen  {:7.2} ms  (layout of visible blocks)", ms(t_first - t_engine));
        println!("next screen   {:7.2} ms  (same, fonts already loaded)", ms(t_warm));
        println!("measure all   {:7.2} ms  (background, off the critical path)", ms(t_all - t_all0));
        println!(
            "scroll step   median {:.3} ms, max {:.3} ms over {} steps (layouts dropped)",
            ms(frames[frames.len() / 2]),
            ms(*frames.last().unwrap()),
            frames.len()
        );
        println!("paint screen  {:7.2} ms  (cairo software; GTK uses the GPU)", ms(t_paint));
        println!("find 'the'    {:7.2} ms  ({n} matches)", ms(t_find));
        return ExitCode::SUCCESS;
    }

    // Load images synchronously, re-laying out until stable.
    for _ in 0..4 {
        e.ensure(scroll, scroll + h);
        let reqs: Vec<_> = std::mem::take(&mut e.img_requests);
        if reqs.is_empty() {
            break;
        }
        for p in reqs {
            let img = gdk::Texture::from_filename(&p).map_or(Img::Failed, Img::Ready);
            e.image_ready(&p, img);
        }
    }
    if select {
        e.select_all();
    }
    let surf = cairo::ImageSurface::create(cairo::Format::ARgb32, w as i32, h as i32).unwrap();
    let cr = cairo::Context::new(&surf).unwrap();
    let bg = e.theme.bg;
    cr.set_source_rgb(bg.red() as f64, bg.green() as f64, bg.blue() as f64);
    let _ = cr.paint();
    e.paint(&mut CairoPainter(&cr), scroll, h);
    drop(cr);
    let out = out.unwrap_or_else(|| "out.png".into());
    let mut f = std::fs::File::create(&out).expect("create output");
    surf.write_to_png(&mut f).expect("write png");
    println!("wrote {out} (doc height {:.0}px)", e.total_height());
    ExitCode::SUCCESS
}
