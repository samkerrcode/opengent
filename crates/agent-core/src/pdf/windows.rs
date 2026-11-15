//! Windows.Data.Pdf rasterizer. WinRT needs an initialized apartment, and the caller's
//! thread may already belong to one of the wrong kind (or to none), so each document is
//! rendered on a short-lived thread that joins the multithreaded apartment itself.

use windows::core::{Error as WinError, Result as WinResult, GUID, HRESULT};
use windows::Data::Pdf::{PdfDocument, PdfPage, PdfPageRenderOptions};
use windows::Graphics::Imaging::BitmapEncoder;
use windows::Storage::Streams::{DataReader, DataWriter, InMemoryRandomAccessStream};
use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_MULTITHREADED};
use windows::UI::Color;

use super::{
    corrupt, encrypted, failed, pixel_size, Budget, PageImage, PdfRasterizer, RenderOptions,
    RenderedPdf,
};
use crate::Result;

/// `HRESULT_FROM_WIN32(ERROR_WRONG_PASSWORD)`: the document needs a password to open.
const WRONG_PASSWORD: HRESULT = HRESULT(0x8007_052B_u32 as i32);

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
        pages.push(render_page(&doc, index, opts)?);
    }
    Ok(RenderedPdf {
        page_count: total,
        pages,
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

fn render_page(doc: &PdfDocument, index: u32, opts: &RenderOptions) -> Result<PageImage> {
    let page = doc.GetPage(index).map_err(|_| corrupt())?;
    let result = (|| {
        // `Size` is the page as displayed. Both destination edges are set so the bitmap
        // can never exceed `long_edge_px` on either axis.
        let size = page.Size().map_err(|_| corrupt())?;
        let (width, height) = pixel_size(size.Width.into(), size.Height.into(), opts.long_edge_px)
            .ok_or_else(corrupt)?;
        let encoded = |encoder: WinResult<GUID>| {
            encoder
                .and_then(|id| encode(&page, width, height, id))
                .map_err(|e: WinError| failed(e.code()))
        };
        let png = encoded(BitmapEncoder::PngEncoderId())?;
        if png.len() <= opts.jpeg_over_bytes {
            return Ok(PageImage {
                mime: "image/png",
                bytes: png,
            });
        }
        Ok(PageImage {
            mime: "image/jpeg",
            bytes: encoded(BitmapEncoder::JpegEncoderId())?,
        })
    })();
    let _ = page.Close();
    result
}

fn encode(page: &PdfPage, width: u32, height: u32, encoder: GUID) -> WinResult<Vec<u8>> {
    let options = PdfPageRenderOptions::new()?;
    options.SetDestinationWidth(width)?;
    options.SetDestinationHeight(height)?;
    options.SetBackgroundColor(Color {
        A: 255,
        R: 255,
        G: 255,
        B: 255,
    })?;
    options.SetBitmapEncoderId(encoder)?;
    let out = InMemoryRandomAccessStream::new()?;
    page.RenderWithOptionsToStreamAsync(&out, &options)?
        .join()?;

    let len = u32::try_from(out.Size()?).map_err(|_| WinError::from(WRONG_SIZE))?;
    let reader = DataReader::CreateDataReader(&out.GetInputStreamAt(0)?)?;
    reader.LoadAsync(len)?.join()?;
    let mut bytes = vec![0; len as usize];
    reader.ReadBytes(&mut bytes)?;
    Ok(bytes)
}

/// `E_OUTOFMEMORY`: an encoded page that does not fit a WinRT buffer.
const WRONG_SIZE: HRESULT = HRESULT(0x8007_000E_u32 as i32);
