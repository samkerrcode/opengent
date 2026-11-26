//! PDF page rasterization. Untrusted PDFs are parsed and drawn by the operating system's
//! own renderer (CoreGraphics on macOS, Windows.Data.Pdf on Windows) behind a small trait,
//! so no PDF parser is linked into the app and tests can substitute a fake. Output is
//! bounded on every axis: page count, pixels per page, encoded size and wall-clock time.

#[cfg(all(feature = "native-pdf", target_os = "macos"))]
mod macos;
#[cfg(all(feature = "native-pdf", target_os = "windows"))]
mod windows;

use std::sync::Arc;
use std::time::Duration;

use crate::Result;

pub const PDF_MIME: &str = "application/pdf";
/// Only files that begin with this marker are treated as PDFs.
pub const PDF_MAGIC: &[u8] = b"%PDF-";

/// Identifies the renderer output stored for a PDF. Bump it whenever `RenderOptions`
/// defaults or the renderers change what they produce, so stored pages are re-rendered
/// the next time the same file is attached.
pub const RENDER_VERSION: u32 = 2;

#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// Pages rendered from the start of the document; the rest are counted but not drawn.
    pub max_pages: u32,
    /// Each page is scaled so its longer edge has at most this many pixels.
    pub long_edge_px: u32,
    /// …and so it has at most about this many pixels in total. Vision models downscale
    /// larger images before looking at them, so pixels beyond this are only extra bytes.
    pub max_pixels: u32,
    /// Longer edge of the page-1 thumbnail shown in the UI.
    pub thumbnail_long_edge_px: u32,
    /// A page whose PNG is larger than this is stored as JPEG instead.
    pub jpeg_over_bytes: usize,
    /// JPEG quality in `0.0..=1.0` (macOS; the Windows encoder uses its default).
    pub jpeg_quality: f32,
    /// Checked between pages only: once spent, rendering stops with the pages finished so
    /// far. It cannot interrupt the OS renderer while it opens the document or draws one
    /// page, so it bounds the number of pages a slow file gets, not the worst case time.
    pub time_budget: Duration,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            max_pages: 20,
            long_edge_px: 1568,
            max_pixels: 1_150_000,
            thumbnail_long_edge_px: 256,
            jpeg_over_bytes: 750 * 1024,
            jpeg_quality: 0.85,
            time_budget: Duration::from_secs(30),
        }
    }
}

impl RenderOptions {
    /// Pages a complete render of a document with `page_count` pages has.
    pub fn expected_pages(&self, page_count: u32) -> u32 {
        page_count.min(self.max_pages)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PageImage {
    pub mime: &'static str,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RenderedPdf {
    /// Pages in the document, which can exceed `pages.len()`.
    pub page_count: u32,
    /// Rendered pages in document order, starting at page 1. Never empty. Fewer than
    /// `RenderOptions::expected_pages` when the time budget ran out.
    pub pages: Vec<PageImage>,
    /// Page 1 at `RenderOptions::thumbnail_long_edge_px`.
    pub thumbnail: PageImage,
}

pub trait PdfRasterizer: Send + Sync {
    /// Renders the leading pages of `bytes` on an opaque white background. Encrypted,
    /// corrupt and empty documents are `Error::InvalidInput` with a user-facing reason.
    fn render(&self, bytes: &[u8], opts: &RenderOptions) -> Result<RenderedPdf>;
}

/// The operating system's renderer, or a stub that rejects every file where there is none.
pub fn native() -> Arc<dyn PdfRasterizer> {
    #[cfg(all(feature = "native-pdf", target_os = "macos"))]
    return Arc::new(macos::CoreGraphicsRasterizer);
    #[cfg(all(feature = "native-pdf", target_os = "windows"))]
    return Arc::new(windows::WinRtRasterizer);
    #[cfg(not(all(
        feature = "native-pdf",
        any(target_os = "macos", target_os = "windows")
    )))]
    Arc::new(Unsupported)
}

#[cfg(not(all(
    feature = "native-pdf",
    any(target_os = "macos", target_os = "windows")
)))]
struct Unsupported;

#[cfg(not(all(
    feature = "native-pdf",
    any(target_os = "macos", target_os = "windows")
)))]
impl PdfRasterizer for Unsupported {
    fn render(&self, _: &[u8], _: &RenderOptions) -> Result<RenderedPdf> {
        Err(crate::Error::InvalidInput(
            "PDF attachments are not supported on this platform".into(),
        ))
    }
}

/// Shared by the native renderers, so only built where there is one.
#[cfg(all(
    feature = "native-pdf",
    any(target_os = "macos", target_os = "windows")
))]
mod support {
    use std::time::Instant;

    use super::RenderOptions;
    use crate::Error;

    pub(super) fn encrypted() -> Error {
        Error::InvalidInput(
            "the PDF is password-protected; remove the password and try again".into(),
        )
    }

    pub(super) fn corrupt() -> Error {
        Error::InvalidInput("the PDF is damaged or not a valid PDF".into())
    }

    pub(super) fn failed(what: impl std::fmt::Display) -> Error {
        Error::Internal(format!("the PDF could not be rendered ({what})"))
    }

    /// Output size for one rendered image: at most `long_edge` pixels on the longer side
    /// and about `max_pixels` in total.
    #[derive(Debug, Clone, Copy)]
    pub(super) struct Target {
        pub long_edge: u32,
        pub max_pixels: u32,
    }

    impl Target {
        pub fn page(opts: &RenderOptions) -> Self {
            Self {
                long_edge: opts.long_edge_px,
                max_pixels: opts.max_pixels,
            }
        }

        pub fn thumbnail(opts: &RenderOptions) -> Self {
            Self {
                long_edge: opts.thumbnail_long_edge_px,
                max_pixels: u32::MAX,
            }
        }

        /// Pixel size for a page of `width` × `height` (any unit), and the scale from page
        /// units to pixels. `None` for empty or non-finite boxes; the shorter edge is at
        /// least one pixel however extreme the aspect ratio.
        pub fn fit(self, width: f64, height: f64) -> Option<(u32, u32, f64)> {
            if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0)
                || self.long_edge == 0
            {
                return None;
            }
            let by_edge = f64::from(self.long_edge) / width.max(height);
            let by_area = (f64::from(self.max_pixels) / (width * height)).sqrt();
            let scale = by_edge.min(by_area);
            let px = |v: f64| ((v * scale).round() as u32).clamp(1, self.long_edge);
            let (pixel_width, pixel_height) = self.fit_pixels(px(width), px(height))?;
            // If integer area enforcement removed a rounded pixel, reduce the drawing
            // scale too so renderers do not clip the page at the bitmap edge.
            let scale = scale
                .min(f64::from(pixel_width) / width)
                .min(f64::from(pixel_height) / height);
            Some((pixel_width, pixel_height, scale))
        }

        /// Constrain a renderer's actual pixel dimensions to this target. Use integer
        /// arithmetic for the final area check so rounding can never exceed the budget.
        pub fn fit_pixels(self, width: u32, height: u32) -> Option<(u32, u32)> {
            if width == 0 || height == 0 || self.long_edge == 0 || self.max_pixels == 0 {
                return None;
            }
            let scale = (f64::from(self.long_edge) / f64::from(width.max(height)))
                .min((f64::from(self.max_pixels) / (f64::from(width) * f64::from(height))).sqrt());
            if scale >= 1.0 {
                return Some((width, height));
            }

            let scaled = |value: u32| (f64::from(value) * scale).floor().max(1.0) as u32;
            let (mut w, mut h) = (scaled(width), scaled(height));
            // Floating-point rounding and the minimum one-pixel edge must never bypass
            // either cap, even for extreme aspect ratios or caller-supplied limits.
            while w.max(h) > self.long_edge
                || u64::from(w) * u64::from(h) > u64::from(self.max_pixels)
            {
                if w >= h && w > 1 {
                    w -= 1;
                } else if h > 1 {
                    h -= 1;
                } else {
                    return None;
                }
            }
            Some((w, h))
        }
    }

    /// Wall-clock budget for one document (`RenderOptions::time_budget`).
    pub(super) struct Budget(Instant);

    impl Budget {
        pub fn start(opts: &RenderOptions) -> Self {
            Self(Instant::now() + opts.time_budget)
        }

        pub fn spent(&self) -> bool {
            Instant::now() >= self.0
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn edge(long_edge: u32) -> Target {
            Target {
                long_edge,
                max_pixels: u32::MAX,
            }
        }

        fn size(t: Target, w: f64, h: f64) -> Option<(u32, u32)> {
            t.fit(w, h).map(|(w, h, _)| (w, h))
        }

        #[test]
        fn fit_scales_the_long_edge_and_rejects_degenerate_boxes() {
            assert_eq!(size(edge(1568), 200.0, 100.0), Some((1568, 784)));
            assert_eq!(size(edge(1568), 612.0, 792.0), Some((1212, 1568)));
            assert_eq!(size(edge(1568), 1.0, 1e9), Some((1, 1568)));
            assert_eq!(size(edge(1568), 0.0, 100.0), None);
            assert_eq!(size(edge(1568), f64::NAN, 100.0), None);
            assert_eq!(size(edge(1568), f64::INFINITY, 100.0), None);
            assert_eq!(size(edge(1568), -5.0, 100.0), None);
        }

        #[test]
        fn fit_caps_the_pixel_area() {
            let page = Target::page(&RenderOptions::default());
            // US Letter: the area cap, not the long edge, decides.
            let (w, h) = size(page, 612.0, 792.0).unwrap();
            assert_eq!((w, h), (942, 1219));
            assert!(w * h <= 1_150_000);
            // A long, thin strip keeps the long-edge cap.
            assert_eq!(size(page, 100.0, 1000.0), Some((157, 1568)));
            let thumb = Target::thumbnail(&RenderOptions::default());
            assert_eq!(size(thumb, 612.0, 792.0), Some((198, 256)));
        }

        #[test]
        fn fit_pixels_bounds_oversized_landscape_and_portrait_without_upscaling() {
            let target = Target {
                long_edge: 100,
                max_pixels: 6_000,
            };
            for (source, expected) in [((360, 180), (100, 50)), ((180, 360), (50, 100))] {
                let size = target.fit_pixels(source.0, source.1).unwrap();
                assert_eq!(size, expected);
                assert!(u64::from(size.0) * u64::from(size.1) <= 6_000);
            }
            assert_eq!(target.fit_pixels(80, 40), Some((80, 40)));
        }

        #[test]
        fn fit_pixels_safely_handles_zero_extreme_and_tight_budgets() {
            let target = Target {
                long_edge: u32::MAX,
                max_pixels: 17,
            };
            let (w, h) = target.fit_pixels(u32::MAX, u32::MAX).unwrap();
            assert!(u64::from(w) * u64::from(h) <= 17);
            assert_eq!(target.fit_pixels(0, 10), None);
            assert_eq!(
                Target {
                    long_edge: 5,
                    max_pixels: 0
                }
                .fit_pixels(10, 2),
                None
            );
        }
    }
}

/// Hand-built PDFs for tests, so no binary fixtures are committed.
#[cfg(test)]
pub(crate) mod fixtures {
    #[derive(Default)]
    pub struct Page {
        pub width: u32,
        pub height: u32,
        pub rotate: u32,
        /// `/CropBox [x0 y0 x1 y1]`; the media box is shown when absent.
        pub crop: Option<[u32; 4]>,
    }

    pub fn page(width: u32, height: u32) -> Page {
        Page {
            width,
            height,
            ..Page::default()
        }
    }

    /// An ASCII-only PDF. Every page is blank except for a red 20 × 20 pt square in the
    /// lower-left corner of its (unrotated) crop box.
    pub fn pdf(pages: &[Page]) -> Vec<u8> {
        build(pages, "")
    }

    /// Declares standard-security encryption with a user password nobody knows.
    pub fn encrypted_pdf() -> Vec<u8> {
        let key = "ab".repeat(32);
        build(
            &[page(200, 100)],
            &format!(
                " /Encrypt << /Filter /Standard /V 1 /R 2 /O <{key}> /U <{key}> /P -64 >> \
                 /ID [<{id}> <{id}>]",
                id = "cd".repeat(16)
            ),
        )
    }

    fn build(pages: &[Page], trailer_extra: &str) -> Vec<u8> {
        let kids: Vec<String> = (0..pages.len())
            .map(|i| format!("{} 0 R", 3 + 2 * i))
            .collect();
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            format!(
                "<< /Type /Pages /Kids [{}] /Count {} >>",
                kids.join(" "),
                pages.len()
            ),
        ];
        for (i, p) in pages.iter().enumerate() {
            let [x, y, ..] = p.crop.unwrap_or_default();
            let crop = p
                .crop
                .map(|[x0, y0, x1, y1]| format!(" /CropBox [{x0} {y0} {x1} {y1}]"))
                .unwrap_or_default();
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}]{crop} /Rotate {} \
                 /Resources << >> /Contents {} 0 R >>",
                p.width,
                p.height,
                p.rotate,
                4 + 2 * i
            ));
            let content = format!("1 0 0 rg {x} {y} 20 20 re f");
            objects.push(format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ));
        }
        let mut out = String::from("%PDF-1.4\n");
        let mut offsets = Vec::with_capacity(objects.len());
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{body}\nendobj\n", i + 1));
        }
        let xref = out.len();
        out.push_str(&format!(
            "xref\n0 {}\n0000000000 65535 f \n",
            objects.len() + 1
        ));
        for offset in offsets {
            out.push_str(&format!("{offset:010} 00000 n \n"));
        }
        out.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R{trailer_extra} >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        ));
        out.into_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_is_ascii_and_starts_with_the_pdf_marker() {
        let pdf = fixtures::pdf(&[fixtures::page(200, 100)]);
        assert!(pdf.starts_with(PDF_MAGIC));
        assert!(pdf.is_ascii());
    }
}

/// Real renders through the OS engine; these run on the macOS and Windows CI runners.
#[cfg(all(
    test,
    feature = "native-pdf",
    any(target_os = "macos", target_os = "windows")
))]
mod native_tests {
    use super::fixtures::{self, page};
    use super::*;
    use crate::Error;

    struct Decoded {
        width: u32,
        height: u32,
        samples: usize,
        data: Vec<u8>,
    }

    impl Decoded {
        /// RGB at a position given as fractions of the width and height from the top-left.
        fn rgb_at(&self, fx: f64, fy: f64) -> [u8; 3] {
            let x = ((f64::from(self.width) * fx) as usize).min(self.width as usize - 1);
            let y = ((f64::from(self.height) * fy) as usize).min(self.height as usize - 1);
            let i = (y * self.width as usize + x) * self.samples;
            [self.data[i], self.data[i + 1], self.data[i + 2]]
        }
    }

    fn decode(image: &PageImage) -> Decoded {
        assert_eq!(image.mime, "image/png");
        let mut decoder = png::Decoder::new(std::io::Cursor::new(&image.bytes));
        decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut reader = decoder.read_info().unwrap();
        let mut data = vec![0; reader.output_buffer_size().unwrap()];
        let frame = reader.next_frame(&mut data).unwrap();
        let samples = frame.color_type.samples();
        assert!(samples >= 3, "expected a colour image");
        Decoded {
            width: frame.width,
            height: frame.height,
            samples,
            data,
        }
    }

    fn is_white([r, g, b]: [u8; 3]) -> bool {
        r > 240 && g > 240 && b > 240
    }

    fn is_red([r, g, b]: [u8; 3]) -> bool {
        r > 200 && g < 60 && b < 60
    }

    /// Default options with only the long-edge cap, so sizes are easy to state.
    fn by_long_edge() -> RenderOptions {
        RenderOptions {
            max_pixels: u32::MAX,
            ..RenderOptions::default()
        }
    }

    #[test]
    fn renders_each_page_at_the_long_edge_on_white() {
        let pdf = fixtures::pdf(&[page(200, 100), page(100, 200)]);
        let out = native().render(&pdf, &by_long_edge()).unwrap();
        assert_eq!(out.page_count, 2);
        assert_eq!(out.pages.len(), 2);

        let landscape = decode(&out.pages[0]);
        assert_eq!((landscape.width, landscape.height), (1568, 784));
        assert!(is_white(landscape.rgb_at(0.5, 0.5)), "background is white");
        assert!(is_red(landscape.rgb_at(0.05, 0.9)), "content is drawn");

        let portrait = decode(&out.pages[1]);
        assert_eq!((portrait.width, portrait.height), (784, 1568));
        assert!(is_white(portrait.rgb_at(0.5, 0.5)));
        assert!(is_red(portrait.rgb_at(0.1, 0.95)));
    }

    #[test]
    fn pages_are_capped_by_area_and_page_one_gets_a_small_thumbnail() {
        let pdf = fixtures::pdf(&[page(612, 792)]);
        let out = native().render(&pdf, &RenderOptions::default()).unwrap();
        let full = decode(&out.pages[0]);
        assert_eq!((full.width, full.height), (942, 1219));
        let thumb = decode(&out.thumbnail);
        assert_eq!((thumb.width, thumb.height), (198, 256));
        assert!(is_red(thumb.rgb_at(0.015, 0.99)), "thumbnail shows page 1");
    }

    #[test]
    fn page_outputs_stay_within_edge_and_pixel_limits() {
        let pdf = fixtures::pdf(&[page(200, 100), page(100, 200)]);
        let opts = RenderOptions {
            long_edge_px: 100,
            max_pixels: 1_600,
            thumbnail_long_edge_px: 40,
            ..RenderOptions::default()
        };
        let out = native().render(&pdf, &opts).unwrap();
        for image in &out.pages {
            let decoded = decode(image);
            assert!(decoded.width.max(decoded.height) <= opts.long_edge_px);
            assert!(
                u64::from(decoded.width) * u64::from(decoded.height) <= u64::from(opts.max_pixels)
            );
            assert!(
                (i64::from(decoded.width) - 2 * i64::from(decoded.height)).abs() <= 1
                    || (2 * i64::from(decoded.width) - i64::from(decoded.height)).abs() <= 1,
                "unexpected aspect ratio for {}x{} output",
                decoded.width,
                decoded.height
            );
        }
    }

    #[test]
    fn stops_at_the_page_cap_but_reports_the_real_count() {
        let pdf = fixtures::pdf(&[page(100, 100), page(100, 100), page(100, 100)]);
        let opts = RenderOptions {
            max_pages: 2,
            long_edge_px: 64,
            ..RenderOptions::default()
        };
        let out = native().render(&pdf, &opts).unwrap();
        assert_eq!(out.page_count, 3);
        assert_eq!(out.pages.len(), 2);
        assert_eq!(decode(&out.pages[0]).width, 64);
    }

    #[test]
    fn a_spent_time_budget_keeps_the_first_page() {
        let pdf = fixtures::pdf(&[page(100, 100), page(100, 100)]);
        let opts = RenderOptions {
            long_edge_px: 64,
            time_budget: Duration::ZERO,
            ..RenderOptions::default()
        };
        let out = native().render(&pdf, &opts).unwrap();
        assert_eq!(out.page_count, 2);
        assert_eq!(out.pages.len(), 1);
    }

    #[test]
    fn oversized_pages_are_stored_as_jpeg() {
        let pdf = fixtures::pdf(&[page(100, 100)]);
        let opts = RenderOptions {
            long_edge_px: 64,
            jpeg_over_bytes: 0,
            ..RenderOptions::default()
        };
        let out = native().render(&pdf, &opts).unwrap();
        assert_eq!(out.pages[0].mime, "image/jpeg");
        assert!(out.pages[0].bytes.starts_with(b"\xff\xd8\xff"));
    }

    #[test]
    fn rejects_files_that_are_not_renderable_pdfs() {
        let renderer = native();
        let opts = RenderOptions::default();
        for bad in [
            b"%PDF-1.4\nthis is not really a PDF\n".to_vec(),
            fixtures::pdf(&[]),
            fixtures::encrypted_pdf(),
        ] {
            let err = renderer.render(&bad, &opts).unwrap_err();
            assert!(matches!(err, Error::InvalidInput(_)), "{err}");
        }
    }

    #[test]
    fn encrypted_pdfs_are_named_as_such() {
        let err = native()
            .render(&fixtures::encrypted_pdf(), &RenderOptions::default())
            .unwrap_err();
        assert!(err.to_string().contains("password"), "{err}");
    }

    /// `/Rotate 90` turns the page clockwise for display: the landscape media box becomes a
    /// portrait image and the lower-left square ends up top-left.
    #[test]
    fn honours_page_rotation() {
        let pdf = fixtures::pdf(&[fixtures::Page {
            rotate: 90,
            ..page(200, 100)
        }]);
        let out = native().render(&pdf, &by_long_edge()).unwrap();
        let image = decode(&out.pages[0]);
        assert_eq!((image.width, image.height), (784, 1568));
        assert!(is_red(image.rgb_at(0.1, 0.05)), "square is top-left");
        assert!(is_white(image.rgb_at(0.1, 0.95)));
        assert!(is_white(image.rgb_at(0.9, 0.05)));
    }

    /// Only the crop box is shown, mapped from its own origin: the square drawn at the
    /// crop box's lower-left corner must land at the image's lower-left corner.
    #[test]
    fn shows_a_crop_box_that_does_not_start_at_the_origin() {
        let pdf = fixtures::pdf(&[fixtures::Page {
            crop: Some([50, 25, 150, 75]),
            ..page(200, 100)
        }]);
        let out = native().render(&pdf, &by_long_edge()).unwrap();
        let image = decode(&out.pages[0]);
        assert_eq!((image.width, image.height), (1568, 784));
        assert!(is_red(image.rgb_at(0.05, 0.9)), "square is bottom-left");
        assert!(is_white(image.rgb_at(0.5, 0.5)));
        assert!(is_white(image.rgb_at(0.95, 0.1)));
    }
}
