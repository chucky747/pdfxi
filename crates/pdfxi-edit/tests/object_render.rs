//! Render-level proof for T-0265 object move and delete.
//!
//! The unit tests prove the content-stream rewrite and the reparsed model. This
//! integration test adds independent readers: PDFium rasterizes the page before
//! and after the edit and the same crop must lose its ink where the object was
//! and gain it where the object moved. PDFium's own character boxes then prove
//! the moved text's baseline actually changed, not only that the text still
//! extracts somewhere on the page.
//!
//! PDFium is loaded at runtime through `PDFIUM_PATH` (debug builds) or the
//! per-user developer cache; a missing library is a clear backend failure.

use pdfxi_core::Document;
use pdfxi_edit::{
    ImageFormat, ObjectKind, ObjectReplacement, PageObject, Rect, TextAlign, add_image,
    add_text_box, delete_object, list_objects, move_object, replace_object, resize_object,
};
use pdfxi_render::pdfium_backend::PdfiumRenderer;
use pdfxi_render::{CancellationToken, PageIndex, RenderedPage, Renderer};
use pdfxi_text::{CancellationToken as TextCancellationToken, TextExtractor};

const PAGE_WIDTH: f32 = 612.0;
const PAGE_HEIGHT: f32 = 792.0;

fn fixture_bytes() -> Vec<u8> {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/realworld/microsoft-excel-2019-sample.pdf");
    std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn find_text(objects: &[PageObject], needle: &str) -> PageObject {
    objects
        .iter()
        .find(|object| object.kind == ObjectKind::TextBlock && object.text_preview.contains(needle))
        .cloned()
        .unwrap_or_else(|| panic!("text object containing {needle:?} was not listed"))
}

fn renderer_for(bytes: &[u8]) -> PdfiumRenderer {
    PdfiumRenderer::from_bytes(bytes.to_vec()).expect("PDFium must open the document")
}

fn render_page(renderer: &PdfiumRenderer, index: usize) -> RenderedPage {
    renderer
        .render_page(
            PageIndex::new(index),
            1.0,
            &CancellationToken::new(),
            &mut |_| {},
        )
        .expect("PDFium must rasterize the page")
}

fn count_matching(page: &RenderedPage, rect: Rect, predicate: impl Fn([u8; 3]) -> bool) -> usize {
    let width = page.width() as usize;
    let height = page.height() as usize;
    let scale_x = page.width() as f32 / PAGE_WIDTH;
    let scale_y = page.height() as f32 / PAGE_HEIGHT;
    let left = ((rect.x0().max(0.0) * scale_x) as usize).min(width);
    let right = ((rect.x1().clamp(0.0, PAGE_WIDTH) * scale_x) as usize).min(width);
    let top = (((PAGE_HEIGHT - rect.y1().clamp(0.0, PAGE_HEIGHT)) * scale_y) as usize).min(height);
    let bottom =
        (((PAGE_HEIGHT - rect.y0().clamp(0.0, PAGE_HEIGHT)) * scale_y) as usize).min(height);
    if left >= right || top >= bottom {
        return 0;
    }
    let pixels = page.pixels();
    let mut count = 0usize;
    for y in top..bottom {
        for x in left..right {
            let index = (y * width + x) * 4;
            let Some(pixel) = pixels.get(index..index + 4) else {
                continue;
            };
            if predicate([pixel[0], pixel[1], pixel[2]]) {
                count += 1;
            }
        }
    }
    count
}

fn dark(pixel: [u8; 3]) -> bool {
    (u32::from(pixel[0]) * 299 + u32::from(pixel[1]) * 587 + u32::from(pixel[2]) * 114) / 1000 < 128
}

fn red(pixel: [u8; 3]) -> bool {
    pixel[0] > 150 && pixel[1] < 120 && pixel[2] < 150
}

fn blue(pixel: [u8; 3]) -> bool {
    pixel[2] > 150 && pixel[0] < 120
}

/// Bounding box of matching ink inside `rect`, in default user space.
fn ink_extent(
    page: &RenderedPage,
    rect: Rect,
    predicate: impl Fn([u8; 3]) -> bool,
) -> Option<Rect> {
    let width = page.width() as usize;
    let height = page.height() as usize;
    let scale_x = page.width() as f32 / PAGE_WIDTH;
    let scale_y = page.height() as f32 / PAGE_HEIGHT;
    let left = ((rect.x0().max(0.0) * scale_x) as usize).min(width);
    let right = ((rect.x1().clamp(0.0, PAGE_WIDTH) * scale_x) as usize).min(width);
    let top = (((PAGE_HEIGHT - rect.y1().clamp(0.0, PAGE_HEIGHT)) * scale_y) as usize).min(height);
    let bottom =
        (((PAGE_HEIGHT - rect.y0().clamp(0.0, PAGE_HEIGHT)) * scale_y) as usize).min(height);
    if left >= right || top >= bottom {
        return None;
    }
    let pixels = page.pixels();
    let mut min_x = usize::MAX;
    let mut min_y = usize::MAX;
    let mut max_x = 0usize;
    let mut max_y = 0usize;
    let mut found = false;
    for y in top..bottom {
        for x in left..right {
            let index = (y * width + x) * 4;
            let Some(pixel) = pixels.get(index..index + 4) else {
                continue;
            };
            if predicate([pixel[0], pixel[1], pixel[2]]) {
                found = true;
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    if !found {
        return None;
    }
    Some(Rect::saturating(
        min_x as f32 / scale_x,
        PAGE_HEIGHT - (max_y + 1) as f32 / scale_y,
        (max_x + 1) as f32 / scale_x,
        PAGE_HEIGHT - min_y as f32 / scale_y,
    ))
}

#[test]
fn moved_text_extracts_and_renders_at_its_new_position() {
    let mut document = Document::from_bytes(fixture_bytes()).expect("parse fixture");
    add_text_box(
        &mut document,
        1,
        Rect::saturating(72.0, 600.0, 420.0, 640.0),
        "Moved by PDFXI",
        14.0,
        TextAlign::Left,
    )
    .expect("add text box");
    let before_objects = list_objects(&document, 1).expect("list before");
    let target = find_text(&before_objects, "Moved by PDFXI");
    let old_crop = Rect::saturating(65.0, 600.0, 430.0, 645.0);
    let new_crop = Rect::saturating(95.0, 560.0, 460.0, 605.0);

    let before_bytes = document.save().expect("save before");
    let before_page = render_page(&renderer_for(&before_bytes), 1);
    let old_ink_before = count_matching(&before_page, old_crop, dark);
    let new_ink_before = count_matching(&before_page, new_crop, dark);
    assert!(
        old_ink_before > 20,
        "the text box must have ink before the move, got {old_ink_before}"
    );
    assert_eq!(new_ink_before, 0, "the destination region must start blank");

    move_object(&mut document, 1, &target.id, 30.0, -40.0).expect("move text block");
    let saved = document.save().expect("save after move");
    let edited = renderer_for(&saved);
    let page = render_page(&edited, 1);
    let old_ink_after = count_matching(&page, old_crop, dark);
    let new_ink_after = count_matching(&page, new_crop, dark);
    assert!(
        old_ink_after <= 2,
        "the old text region must be empty after the move, got {old_ink_after} ink pixels"
    );
    assert!(
        new_ink_after > 20,
        "the moved text must paint its destination, got {new_ink_after} ink pixels"
    );

    let text_page = edited
        .extract_page(1, &TextCancellationToken::new(), &mut |_| {})
        .expect("PDFium text extraction");
    assert!(text_page.text().contains("Moved by PDFXI"));
    let span = text_page
        .spans()
        .iter()
        .find(|span| span.text.contains("Moved by PDFXI"))
        .expect("PDFium must report the moved text span");
    assert!(
        (span.x - (target.rect.x0() + 30.0)).abs() < 2.0,
        "moved span x was {}, expected about {}",
        span.x,
        target.rect.x0() + 30.0
    );
    assert!(
        (span.y - (target.rect.y0() + 14.0 * 0.2 - 40.0)).abs() < 3.0,
        "moved span baseline was {}, expected about {}",
        span.y,
        target.rect.y0() + 14.0 * 0.2 - 40.0
    );
}

#[test]
fn moved_image_renders_at_its_new_position() {
    let mut document = Document::from_bytes(fixture_bytes()).expect("parse fixture");
    let placement = add_image(
        &mut document,
        1,
        Rect::saturating(72.0, 300.0, 172.0, 400.0),
        &tiny_png(),
        ImageFormat::Png,
    )
    .expect("add image");
    let before_objects = list_objects(&document, 1).expect("list before");
    let target = before_objects
        .iter()
        .find(|object| object.kind == ObjectKind::Image)
        .expect("image object")
        .clone();
    assert_eq!(target.id, format!("image:{}:0", placement.resource));
    let old_crop = Rect::saturating(72.0, 300.0, 172.0, 400.0);
    let new_crop = Rect::saturating(222.0, 250.0, 322.0, 350.0);

    let before_bytes = document.save().expect("save before");
    let before_page = render_page(&renderer_for(&before_bytes), 1);
    assert!(
        count_matching(&before_page, old_crop, red) > 500,
        "the image must be red before the move"
    );
    assert_eq!(count_matching(&before_page, new_crop, red), 0);

    move_object(&mut document, 1, &target.id, 150.0, -50.0).expect("move image");
    let saved = document.save().expect("save after move");
    let page = render_page(&renderer_for(&saved), 1);
    assert_eq!(
        count_matching(&page, old_crop, red),
        0,
        "the old image rectangle must be empty"
    );
    assert!(
        count_matching(&page, new_crop, red) > 500,
        "the moved image must paint its destination"
    );
}

#[test]
fn deleted_text_and_image_leave_no_content() {
    let mut document = Document::from_bytes(fixture_bytes()).expect("parse fixture");
    add_text_box(
        &mut document,
        1,
        Rect::saturating(72.0, 540.0, 300.0, 580.0),
        "Keep me",
        14.0,
        TextAlign::Left,
    )
    .expect("add kept text");
    add_text_box(
        &mut document,
        1,
        Rect::saturating(72.0, 600.0, 300.0, 640.0),
        "Delete me",
        14.0,
        TextAlign::Left,
    )
    .expect("add deleted text");
    add_image(
        &mut document,
        1,
        Rect::saturating(320.0, 540.0, 420.0, 640.0),
        &tiny_png(),
        ImageFormat::Png,
    )
    .expect("add image");
    let objects = list_objects(&document, 1).expect("list");
    let text_target = find_text(&objects, "Delete me");
    let image_target = objects
        .iter()
        .find(|object| object.kind == ObjectKind::Image)
        .expect("image object")
        .clone();
    let deleted_text_crop = Rect::saturating(65.0, 600.0, 300.0, 645.0);
    let deleted_image_crop = Rect::saturating(320.0, 540.0, 420.0, 640.0);
    let kept_text_crop = Rect::saturating(65.0, 540.0, 300.0, 585.0);

    delete_object(&mut document, 1, &text_target.id).expect("delete text");
    delete_object(&mut document, 1, &image_target.id).expect("delete image");
    let saved = document.save().expect("save after delete");
    let edited = renderer_for(&saved);
    let page = render_page(&edited, 1);
    assert_eq!(
        count_matching(&page, deleted_text_crop, dark),
        0,
        "deleted text must leave no ink"
    );
    assert_eq!(
        count_matching(&page, deleted_image_crop, red),
        0,
        "deleted image must leave no ink"
    );
    assert!(
        count_matching(&page, kept_text_crop, dark) > 20,
        "unrelated text must still render"
    );

    let reparse = Document::from_bytes(saved).expect("reparse");
    let text = reparse.page_text(1).expect("text");
    assert!(text.contains("Keep me"));
    assert!(!text.contains("Delete me"));
}

#[test]
fn resized_text_and_image_render_at_scaled_bounds() {
    let mut document = Document::from_bytes(fixture_bytes()).expect("parse fixture");
    add_text_box(
        &mut document,
        1,
        Rect::saturating(72.0, 540.0, 272.0, 584.0),
        "Resize this paragraph and keep every word extractable",
        12.0,
        TextAlign::Left,
    )
    .expect("add text to resize");
    add_image(
        &mut document,
        1,
        Rect::saturating(72.0, 300.0, 172.0, 400.0),
        &tiny_png(),
        ImageFormat::Png,
    )
    .expect("add image to resize");
    let objects = list_objects(&document, 1).expect("list");
    let text = find_text(&objects, "Resize this paragraph");
    let image = objects
        .iter()
        .find(|object| object.kind == ObjectKind::Image)
        .expect("image object")
        .clone();
    let text_window = Rect::saturating(
        text.rect.x0() - 5.0,
        text.rect.y0() - 5.0,
        text.rect.x1() + 5.0,
        text.rect.y1() + 5.0,
    );
    let old_image_band = Rect::saturating(72.0, 355.0, 222.0, 400.0);
    let new_image_strip = Rect::saturating(175.0, 300.0, 220.0, 350.0);

    let before = render_page(&renderer_for(&document.save().expect("save before")), 1);
    let text_before =
        ink_extent(&before, text_window, dark).expect("the added text must have ink before");
    assert!(
        count_matching(&before, old_image_band, red) > 500,
        "the full-height image must cover the top band before the resize"
    );
    assert_eq!(count_matching(&before, new_image_strip, red), 0);

    resize_object(&mut document, 1, &text.id, 1.5, 1.5).expect("resize text");
    resize_object(&mut document, 1, &image.id, 1.5, 0.5).expect("resize image");
    let saved = document.save().expect("save after resize");
    let after = render_page(&renderer_for(&saved), 1);
    let text_after_window = Rect::saturating(
        text.rect.x0() - 8.0,
        text.rect.y0() - 8.0,
        text.rect.x0() + text.rect.width() * 1.5 + 8.0,
        text.rect.y0() + text.rect.height() * 1.5 + 8.0,
    );
    let text_after =
        ink_extent(&after, text_after_window, dark).expect("the resized text must still have ink");
    let width_ratio = text_after.width() / text_before.width();
    let height_ratio = text_after.height() / text_before.height();
    println!(
        "resize text: before={:?} after={:?} width_ratio={width_ratio:.3} height_ratio={height_ratio:.3}",
        text_before, text_after
    );
    assert!(
        (width_ratio - 1.5).abs() < 0.12,
        "text ink width ratio was {width_ratio:.3}, expected about 1.5"
    );
    assert!(
        (height_ratio - 1.5).abs() < 0.15,
        "text ink height ratio was {height_ratio:.3}, expected about 1.5"
    );
    assert!(
        (text_after.x0() - text_before.x0()).abs() < 2.0,
        "the resized text must keep its left edge, got {} vs {}",
        text_after.x0(),
        text_before.x0()
    );
    assert!(
        (text_after.y0() - text_before.y0()).abs() < 2.5,
        "the resized text must keep its bottom edge, got {} vs {}",
        text_after.y0(),
        text_before.y0()
    );

    assert_eq!(
        count_matching(&after, old_image_band, red),
        0,
        "the shrunk image must vacate the band above its new top"
    );
    assert!(
        count_matching(&after, new_image_strip, red) > 100,
        "the widened image must paint the strip beyond its old right edge"
    );
    println!(
        "resize image: band_after={} strip_after={}",
        count_matching(&after, old_image_band, red),
        count_matching(&after, new_image_strip, red)
    );

    let resized = list_objects(&Document::from_bytes(saved).expect("reparse"), 1).expect("list");
    let resized_image = resized
        .iter()
        .find(|object| object.kind == ObjectKind::Image)
        .expect("resized image");
    assert!((resized_image.rect.width() - image.rect.width() * 1.5).abs() < 0.5);
    assert!((resized_image.rect.height() - image.rect.height() * 0.5).abs() < 0.5);
    assert!((resized_image.rect.x0() - image.rect.x0()).abs() < 0.5);
    assert!((resized_image.rect.y0() - image.rect.y0()).abs() < 0.5);
}

#[test]
fn replaced_text_and_image_extract_and_render() {
    let mut document = Document::from_bytes(fixture_bytes()).expect("parse fixture");
    add_text_box(
        &mut document,
        1,
        Rect::saturating(72.0, 540.0, 272.0, 584.0),
        "Before replacement words",
        12.0,
        TextAlign::Left,
    )
    .expect("add text to replace");
    add_image(
        &mut document,
        1,
        Rect::saturating(72.0, 300.0, 172.0, 400.0),
        &tiny_png(),
        ImageFormat::Png,
    )
    .expect("add image to replace");
    let objects = list_objects(&document, 1).expect("list");
    let text = find_text(&objects, "Before replacement");
    let image = objects
        .iter()
        .find(|object| object.kind == ObjectKind::Image)
        .expect("image object")
        .clone();
    let text_crop = Rect::saturating(65.0, 535.0, 280.0, 590.0);
    let image_crop = Rect::saturating(72.0, 300.0, 172.0, 400.0);

    let before = render_page(&renderer_for(&document.save().expect("save before")), 1);
    assert!(count_matching(&before, text_crop, dark) > 20);
    assert!(count_matching(&before, image_crop, red) > 500);
    assert_eq!(count_matching(&before, image_crop, blue), 0);

    replace_object(
        &mut document,
        1,
        &text.id,
        ObjectReplacement::Text("After replacement words extract here"),
    )
    .expect("replace text");
    replace_object(
        &mut document,
        1,
        &image.id,
        ObjectReplacement::Image {
            bytes: &solid_png([20, 120, 220]),
            format: ImageFormat::Png,
        },
    )
    .expect("replace image");

    let saved = document.save().expect("save after replace");
    let edited = renderer_for(&saved);
    let page = render_page(&edited, 1);
    assert!(
        count_matching(&page, text_crop, dark) > 20,
        "the replacement text must paint the block"
    );
    assert_eq!(
        count_matching(&page, image_crop, red),
        0,
        "the replaced image must not keep its old red pixels"
    );
    assert!(
        count_matching(&page, image_crop, blue) > 500,
        "the replacement image must paint blue at the old placement"
    );
    println!(
        "replace: text_dark={} image_blue={} image_red={}",
        count_matching(&page, text_crop, dark),
        count_matching(&page, image_crop, blue),
        count_matching(&page, image_crop, red)
    );

    let extracted = edited
        .extract_page(1, &TextCancellationToken::new(), &mut |_| {})
        .expect("PDFium text extraction");
    assert!(
        extracted.text().contains("After replacement words"),
        "extracted text was {:?}",
        extracted.text()
    );
    assert!(!extracted.text().contains("Before replacement words"));

    let reparse = Document::from_bytes(saved).expect("reparse");
    let replaced = list_objects(&reparse, 1).expect("list after replace");
    let replaced_image = replaced
        .iter()
        .find(|object| object.kind == ObjectKind::Image)
        .expect("replaced image");
    assert!((replaced_image.rect.x0() - image.rect.x0()).abs() < 0.01);
    assert!((replaced_image.rect.y0() - image.rect.y0()).abs() < 0.01);
    assert!((replaced_image.rect.width() - image.rect.width()).abs() < 0.01);
    assert!((replaced_image.rect.height() - image.rect.height()).abs() < 0.01);
}

/// A deterministic solid 8x8 red RGB PNG.
fn tiny_png() -> Vec<u8> {
    solid_png([220, 40, 90])
}

/// A deterministic solid 8x8 RGB PNG of one colour.
fn solid_png(color: [u8; 3]) -> Vec<u8> {
    let mut raw = Vec::new();
    for _ in 0..8 {
        raw.resize(raw.len() + 1, 0);
        for _ in 0..8 {
            raw.extend_from_slice(&color);
        }
    }
    let mut zlib = vec![0x78, 0x01];
    let length = raw.len() as u16;
    zlib.push(1);
    zlib.extend_from_slice(&length.to_le_bytes());
    zlib.extend_from_slice(&(!length).to_le_bytes());
    zlib.extend_from_slice(&raw);
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&8u32.to_be_bytes());
    ihdr.extend_from_slice(&8u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    push_chunk(&mut png, b"IHDR", &ihdr);
    push_chunk(&mut png, b"IDAT", &zlib);
    push_chunk(&mut png, b"IEND", &[]);
    png
}

fn push_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    png.extend_from_slice(&[0, 0, 0, 0]);
}

fn adler32(data: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    for byte in data {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}
