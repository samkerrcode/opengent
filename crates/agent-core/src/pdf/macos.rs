//! CoreGraphics + ImageIO rasterizer. CoreGraphics is safe to drive from a background
//! thread; everything it autoreleases is drained per page so memory stays flat.

use std::ptr::NonNull;

use objc2::rc::autoreleasepool;
use objc2_core_foundation::{
    CFData, CFDictionary, CFMutableData, CFNumber, CFString, CGPoint, CGRect, CGSize,
};
use objc2_core_graphics::{
    kCGColorSpaceSRGB, CGBitmapContextCreate, CGBitmapContextCreateImage, CGColorSpace, CGContext,
    CGDataProvider, CGImage, CGImageAlphaInfo, CGInterpolationQuality, CGPDFBox, CGPDFDocument,
    CGPDFPage,
};
use objc2_image_io::{kCGImageDestinationLossyCompressionQuality, CGImageDestination};

use super::support::{corrupt, encrypted, failed, Budget, Target};
use super::{PageImage, PdfRasterizer, RenderOptions, RenderedPdf};
use crate::Result;

pub(super) struct CoreGraphicsRasterizer;

impl PdfRasterizer for CoreGraphicsRasterizer {
    fn render(&self, bytes: &[u8], opts: &RenderOptions) -> Result<RenderedPdf> {
        autoreleasepool(|_| render(bytes, opts))
    }
}

fn render(bytes: &[u8], opts: &RenderOptions) -> Result<RenderedPdf> {
    // The document reads lazily from its provider, so it gets its own copy of the bytes
    // rather than a pointer into a buffer whose lifetime CoreGraphics cannot see.
    let data = CFData::from_bytes(bytes);
    let provider = CGDataProvider::with_cf_data(Some(&data)).ok_or_else(corrupt)?;
    let doc = CGPDFDocument::with_provider(Some(&provider)).ok_or_else(corrupt)?;
    let doc = Some(&*doc);

    // Owner-password-only files open with the empty user password; anything that needs a
    // real password is rejected rather than prompted for.
    if CGPDFDocument::is_encrypted(doc) && !CGPDFDocument::is_unlocked(doc) {
        let empty = NonNull::from(c"").cast();
        // SAFETY: `empty` points at a NUL-terminated string that outlives the call.
        if !unsafe { CGPDFDocument::unlock_with_password(doc, empty) } {
            return Err(encrypted());
        }
    }
    let total = CGPDFDocument::number_of_pages(doc);
    if total == 0 {
        return Err(corrupt());
    }

    // SAFETY: `kCGColorSpaceSRGB` is an immutable constant exported by CoreGraphics.
    let space = CGColorSpace::with_name(Some(unsafe { kCGColorSpaceSRGB }))
        .ok_or_else(|| failed("no sRGB colour space"))?;
    let budget = Budget::start(opts);
    let mut pages = Vec::new();
    let mut thumbnail = None;
    for number in 1..=total.min(opts.max_pages as usize) {
        if number > 1 && budget.spent() {
            break;
        }
        pages.push(autoreleasepool(|_| {
            let page = CGPDFDocument::page(doc, number).ok_or_else(corrupt)?;
            if number == 1 {
                thumbnail = Some(render_page(&page, &space, Target::thumbnail(opts), opts)?);
            }
            render_page(&page, &space, Target::page(opts), opts)
        })?);
    }
    Ok(RenderedPdf {
        page_count: u32::try_from(total).unwrap_or(u32::MAX),
        pages,
        thumbnail: thumbnail.ok_or_else(corrupt)?,
    })
}

fn render_page(
    page: &CGPDFPage,
    space: &CGColorSpace,
    target: Target,
    opts: &RenderOptions,
) -> Result<PageImage> {
    let page = Some(page);
    // The crop box is what a viewer shows (CoreGraphics clamps it to the media box).
    let crop = CGPDFPage::box_rect(page, CGPDFBox::CropBox);
    let rotation = CGPDFPage::rotation_angle(page).rem_euclid(360);
    let (shown_w, shown_h) = if rotation == 90 || rotation == 270 {
        (crop.size.height, crop.size.width)
    } else {
        (crop.size.width, crop.size.height)
    };
    let (width, height, scale) = target.fit(shown_w, shown_h).ok_or_else(corrupt)?;

    // Opaque RGBX: transparent PDF backgrounds come out white, and the PNG has no alpha.
    // SAFETY: a null `data` asks CoreGraphics to allocate and own the pixel buffer.
    let ctx = unsafe {
        CGBitmapContextCreate(
            std::ptr::null_mut(),
            width as usize,
            height as usize,
            8,
            0,
            Some(space),
            CGImageAlphaInfo::NoneSkipLast.0,
        )
    }
    .ok_or_else(|| failed("no bitmap context"))?;
    let ctx = Some(&*ctx);
    let (w, h) = (f64::from(width), f64::from(height));
    CGContext::set_rgb_fill_color(ctx, 1.0, 1.0, 1.0, 1.0);
    CGContext::fill_rect(ctx, CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(w, h)));
    CGContext::set_interpolation_quality(ctx, CGInterpolationQuality::High);

    // Map the crop box onto the bitmap: centre it, apply the page's clockwise `/Rotate`,
    // and scale. (`CGPDFPageGetDrawingTransform` would do this but never scales up.)
    CGContext::translate_ctm(ctx, w / 2.0, h / 2.0);
    CGContext::rotate_ctm(ctx, -f64::from(rotation).to_radians());
    CGContext::scale_ctm(ctx, scale, scale);
    CGContext::translate_ctm(
        ctx,
        -(crop.origin.x + crop.size.width / 2.0),
        -(crop.origin.y + crop.size.height / 2.0),
    );
    CGContext::clip_to_rect(ctx, crop);
    CGContext::draw_pdf_page(ctx, page);

    let image = CGBitmapContextCreateImage(ctx).ok_or_else(|| failed("no image"))?;
    let png = encode(&image, "public.png", None)?;
    if png.len() <= opts.jpeg_over_bytes {
        return Ok(PageImage {
            mime: "image/png",
            bytes: png,
        });
    }
    Ok(PageImage {
        mime: "image/jpeg",
        bytes: encode(&image, "public.jpeg", Some(opts.jpeg_quality))?,
    })
}

fn encode(image: &CGImage, uti: &'static str, quality: Option<f32>) -> Result<Vec<u8>> {
    let out = CFMutableData::new(None, 0).ok_or_else(|| failed("out of memory"))?;
    let uti = CFString::from_static_str(uti);
    let properties = quality.map(|q| {
        let quality = CFNumber::new_f64(f64::from(q.clamp(0.0, 1.0)));
        // SAFETY: the key is an immutable constant exported by ImageIO.
        let key = unsafe { kCGImageDestinationLossyCompressionQuality };
        CFDictionary::<CFString, CFNumber>::from_slices(&[key], &[&*quality])
    });
    // SAFETY: no creation options are passed; the properties dictionary holds the one
    // key ImageIO documents for lossy quality, with the CFNumber value it expects.
    let finalized = unsafe {
        let destination = CGImageDestination::with_data(&out, &uti, 1, None)
            .ok_or_else(|| failed("no image encoder"))?;
        destination.add_image(image, properties.as_deref().map(|p| p.as_opaque()));
        destination.finalize()
    };
    if !finalized {
        return Err(failed("image encoding failed"));
    }
    Ok(out.to_vec())
}
