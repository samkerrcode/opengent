//! Windows.Data.Pdf rasterizer. WinRT needs an initialized apartment, and the caller's
//! thread may already belong to one of the wrong kind (or to none), so each document is
//! rendered on a short-lived thread that joins the multithreaded apartment itself.

use windows::core::{Error as WinError, Result as WinResult, HRESULT};
use windows::Data::Pdf::{PdfDocument, PdfPage, PdfPageRenderOptions};
use windows::Graphics::Imaging::{BitmapDecoder, BitmapEncoder};
use windows::Storage::Streams::{DataReader, DataWriter, InMemoryRandomAccessStream};
use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};
use windows::UI::Color;

use super::support::{corrupt, encrypted, failed, Budget, Target};
use super::{PageImage, PdfRasterizer, RenderOptions, RenderedPdf};
use crate::Result;

/// `HRESULT_FROM_WIN32(ERROR_WRONG_PASSWORD)`: the document needs a password to open.
const WRONG_PASSWORD: HRESULT = HRESULT(0x8007_052B_u32 as i32);
/// `E_OUTOFMEMORY`: an encoded page that does not fit a WinRT buffer.
const E_OUTOFMEMORY: HRESULT = HRESULT(0x8007_000E_u32 as i32);

pub(super) struct WinRtRasterizer;

impl PdfRasterizer for WinRtRasterizer {
    fn render(&self, bytes: &[u8], opts: &RenderOptions) -> Result<RenderedPdf> {
        std::thread::scope(|scope| {
            std::thread::Builder::new()
                .name("pdf-render".into())
                .spawn_scoped(scope, || {
                    // Declared first so it is dropped last, after every WinRT object.
                    let _apartment = Apartment::enter()?;
                    render(bytes, opts)
                })?
                .join()
                .unwrap_or_else(|_| Err(failed("the renderer stopped unexpectedly")))
        })
    }
}

struct Apartment;

impl Apartment {
    fn enter() -> Result<Self> {
        // SAFETY: paired with `RoUninitialize` in `Drop`, on the same thread.
        unsafe { RoInitialize(RO_INIT_MULTITHREADED) }.map_err(|e| failed(e.code()))?;
        Ok(Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: balances the successful `RoInitialize` in `enter`.
        unsafe { RoUninitialize() };
    }
}

fn render(bytes: &[u8], opts: &RenderOptions) -> Result<RenderedPdf> {
    let doc = load(bytes).map_err(|e| {
        if e.code() == WRONG_PASSWORD {
            encrypted()
        } else {
            corrupt()
        }
    })?;
    let total = doc.PageCount().map_err(|_| corrupt())?;
    if total == 0 {
        return Err(corrupt());
    }
    let budget = Budget::start(opts);
    let mut pages = Vec::new();
    for index in 0..total.min(opts.max_pages) {
        if index > 0 && budget.spent() {
            break;
        }
        pages.push(render_page(&doc, index, Target::page(opts), opts)?);
    }
    Ok(RenderedPdf {
        page_count: total,
        pages,
        thumbnail: render_page(&doc, 0, Target::thumbnail(opts), opts)?,
    })
}

fn load(bytes: &[u8]) -> WinResult<PdfDocument> {
    let stream = InMemoryRandomAccessStream::new()?;
    let writer = DataWriter::CreateDataWriter(&stream)?;
    writer.WriteBytes(bytes)?;
    writer.StoreAsync()?.join()?;
    writer.FlushAsync()?.join()?;
    writer.DetachStream()?;
    stream.Seek(0)?;
    PdfDocument::LoadFromStreamAsync(&stream)?.join()
}

fn render_page(
    doc: &PdfDocument,
    index: u32,
    target: Target,
    opts: &RenderOptions,
) -> Result<PageImage> {
    let page = doc.GetPage(index).map_err(|_| corrupt())?;
    let result = (|| {
        // `Size` is the page as displayed. Both destination edges are set so the bitmap
        // can never exceed the target on either axis.
        let size = page.Size().map_err(|_| corrupt())?;
        let (width, height, _) = target
            .fit(size.Width.into(), size.Height.into())
            .ok_or_else(corrupt)?;
        let win = |e: WinError| failed(e.code());
        let png = render_png(&page, width, height).map_err(win)?;
        let bytes = read_all(&png).map_err(win)?;
        if bytes.len() <= opts.jpeg_over_bytes {
            return Ok(PageImage {
                mime: "image/png",
                bytes,
            });
        }
        // Re-encode the rendered bitmap rather than rasterizing the page a second time.
        Ok(PageImage {
            mime: "image/jpeg",
            bytes: to_jpeg(&png).map_err(win)?,
        })
    })();
    let _ = page.Close();
    result
}

fn render_png(page: &PdfPage, width: u32, height: u32) -> WinResult<InMemoryRandomAccessStream> {
    let options = PdfPageRenderOptions::new()?;
    options.SetDestinationWidth(width)?;
    options.SetDestinationHeight(height)?;
    options.SetBackgroundColor(Color {
        A: 255,
        R: 255,
        G: 255,
        B: 255,
    })?;
    options.SetBitmapEncoderId(BitmapEncoder::PngEncoderId()?)?;
    let out = InMemoryRandomAccessStream::new()?;
    page.RenderWithOptionsToStreamAsync(&out, &options)?
        .join()?;
    Ok(out)
}

fn to_jpeg(png: &InMemoryRandomAccessStream) -> WinResult<Vec<u8>> {
    png.Seek(0)?;
    let bitmap = BitmapDecoder::CreateAsync(png)?
        .join()?
        .GetSoftwareBitmapAsync()?
        .join()?;
    let out = InMemoryRandomAccessStream::new()?;
    let encoder = BitmapEncoder::CreateAsync(BitmapEncoder::JpegEncoderId()?, &out)?.join()?;
    encoder.SetSoftwareBitmap(&bitmap)?;
    encoder.FlushAsync()?.join()?;
    read_all(&out)
}

fn read_all(stream: &InMemoryRandomAccessStream) -> WinResult<Vec<u8>> {
    let len = u32::try_from(stream.Size()?).map_err(|_| WinError::from(E_OUTOFMEMORY))?;
    let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0)?)?;
    reader.LoadAsync(len)?.join()?;
    let mut bytes = vec![0; len as usize];
    reader.ReadBytes(&mut bytes)?;
    Ok(bytes)
}
