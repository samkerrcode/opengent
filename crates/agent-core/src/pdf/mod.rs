//! PDF page rasterization. Untrusted PDFs are parsed and drawn by the operating system's
//! own renderer (CoreGraphics on macOS, Windows.Data.Pdf on Windows) behind a small trait,
//! so no PDF parser is linked into the app and tests can substitute a fake. Output is
//! bounded on every axis: page count, pixels per page, encoded size and wall-clock time.

#[cfg(all(feature = "native-pdf", target_os = "macos"))]
mod macos;
#[cfg(all(feature = "native-pdf", target_os = "windows"))]
mod windows;

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::{Error, Result};

pub const PDF_MIME: &str = "application/pdf";
/// Only files that begin with this marker are treated as PDFs.
pub const PDF_MAGIC: &[u8] = b"%PDF-";

#[derive(Debug, Clone)]
pub struct RenderOptions {
    /// Pages rendered from the start of the document; the rest are counted but not drawn.
    pub max_pages: u32,
    /// Each page is scaled so its longer edge has this many pixels.
    pub long_edge_px: u32,
    /// A page whose PNG is larger than this is stored as JPEG instead.
    pub jpeg_over_bytes: usize,
    /// JPEG quality in `0.0..=1.0` (macOS; the Windows encoder uses its default).
    pub jpeg_quality: f32,
    /// Checked between pages: once spent, rendering stops with the pages finished so far,
    /// so a hostile or enormous file cannot hold a blocking thread indefinitely.
    pub time_budget: Duration,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            max_pages: 20,
            long_edge_px: 1568,
            jpeg_over_bytes: 750 * 1024,
            jpeg_quality: 0.85,
            time_budget: Duration::from_secs(30),
        }
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
    /// Rendered pages in document order, starting at page 1. Never empty.
    pub pages: Vec<PageImage>,
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
        Err(Error::InvalidInput(
            "PDF attachments are not supported on this platform".into(),
        ))
    }
}

#[allow(dead_code)]
fn encrypted() -> Error {
    Error::InvalidInput("the PDF is password-protected; remove the password and try again".into())
}

#[allow(dead_code)]
fn corrupt() -> Error {
    Error::InvalidInput("the PDF is damaged or not a valid PDF".into())
}

#[allow(dead_code)]
fn failed(what: impl std::fmt::Display) -> Error {
    Error::Internal(format!("the PDF could not be rendered ({what})"))
}

/// Pixel size for a page of `width` × `height` (any unit) whose longer edge becomes
/// `long_edge` pixels. `None` for empty or non-finite boxes; the shorter edge is at least
/// one pixel however extreme the aspect ratio.
#[allow(dead_code)]
fn pixel_size(width: f64, height: f64, long_edge: u32) -> Option<(u32, u32)> {
    if !(width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0) || long_edge == 0 {
        return None;
    }
    let scale = f64::from(long_edge) / width.max(height);
    let px = |v: f64| ((v * scale).round() as u32).clamp(1, long_edge);
    Some((px(width), px(height)))
}

/// Wall-clock budget for one document (`RenderOptions::time_budget`).
#[allow(dead_code)]
struct Budget(Instant);

#[allow(dead_code)]
impl Budget {
    fn start(opts: &RenderOptions) -> Self {
        Self(Instant::now() + opts.time_budget)
    }

    fn spent(&self) -> bool {
        Instant::now() >= self.0
    }
}

/// Hand-built PDFs for tests, so no binary fixtures are committed.
#[cfg(test)]
pub(crate) mod fixtures {
    pub struct Page {
        pub width: u32,
        pub height: u32,
        pub rotate: u32,
    }

    pub fn page(width: u32, height: u32) -> Page {
        Page {
            width,
            height,
            rotate: 0,
        }
    }

    /// An ASCII-only PDF. Every page is blank except for a red 20 × 20 pt square in the
    /// lower-left corner of its (unrotated) media box.
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
        const CONTENT: &str = "1 0 0 rg 0 0 20 20 re f";
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
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Rotate {} \
                 /Resources << >> /Contents {} 0 R >>",
                p.width,
                p.height,
                p.rotate,
                4 + 2 * i
            ));
            objects.push(format!(
                "<< /Length {} >>\nstream\n{CONTENT}\nendstream",
                CONTENT.len()
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
    fn pixel_size_scales_the_long_edge_and_rejects_degenerate_boxes() {
        assert_eq!(pixel_size(200.0, 100.0, 1568), Some((1568, 784)));
        assert_eq!(pixel_size(612.0, 792.0, 1568), Some((1212, 1568)));
        assert_eq!(pixel_size(1.0, 1e9, 1568), Some((1, 1568)));
        assert_eq!(pixel_size(0.0, 100.0, 1568), None);
        assert_eq!(pixel_size(f64::NAN, 100.0, 1568), None);
        assert_eq!(pixel_size(f64::INFINITY, 100.0, 1568), None);
        assert_eq!(pixel_size(-5.0, 100.0, 1568), None);
    }

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

    #[test]
    fn renders_each_page_at_the_long_edge_on_white() {
        let pdf = fixtures::pdf(&[page(200, 100), page(100, 200)]);
        let out = native().render(&pdf, &RenderOptions::default()).unwrap();
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

    #[cfg(target_os = "macos")]
    #[test]
    fn encrypted_pdfs_are_named_as_such() {
        let err = native()
            .render(&fixtures::encrypted_pdf(), &RenderOptions::default())
            .unwrap_err();
        assert!(err.to_string().contains("password"), "{err}");
    }

    /// `/Rotate 90` turns the page clockwise for display: the landscape media box becomes a
    /// portrait image and the lower-left square ends up top-left.
    #[cfg(target_os = "macos")]
    #[test]
    fn honours_page_rotation() {
        let pdf = fixtures::pdf(&[fixtures::Page {
            width: 200,
            height: 100,
            rotate: 90,
        }]);
        let out = native().render(&pdf, &RenderOptions::default()).unwrap();
        let image = decode(&out.pages[0]);
        assert_eq!((image.width, image.height), (784, 1568));
        assert!(is_red(image.rgb_at(0.1, 0.05)), "square is top-left");
        assert!(is_white(image.rgb_at(0.1, 0.95)));
        assert!(is_white(image.rgb_at(0.9, 0.05)));
    }
}
