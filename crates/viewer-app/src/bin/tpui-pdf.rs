//! `tpui-pdf`, the viewer's PDF worker. It reads page sizes and draws pages with the PDF renderer of Windows
//! (`Windows.Data.Pdf`), so that the renderer's memory and threads live in this process and end with it.
//!
//! The viewer starts it while a PDF tab is shown and talks over stdin and stdout in the frames of `viewer_core`'s PDF
//! messages. When stdin closes, the viewer is gone and this process ends at once, even in the middle of a page. A file is
//! open only while its sizes are read or its pages drawn: an open document can keep other programs from rewriting it.

#[cfg(target_os = "windows")]
fn main() {
    worker::run();
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("tpui-pdf draws PDF pages with the renderer of Windows");
    std::process::exit(1);
}

/// Reads a 32-bit BMP, the form the renderer is asked to answer in. Rows stored from the bottom are turned around.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn decode_bmp(bytes: &[u8]) -> Result<viewer_core::PdfPageImage, String> {
    let bad = |reason: &str| format!("unexpected page image: {reason}");
    let u16_at = |at: usize| -> Option<u16> {
        Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
    };
    let u32_at = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
    };
    if bytes.get(..2) != Some(b"BM") {
        return Err(bad("not a BMP"));
    }
    let header = || bad("short header");
    let offset = u32_at(10).ok_or_else(header)? as usize;
    let width = u32_at(18).ok_or_else(header)? as i32;
    let height = u32_at(22).ok_or_else(header)? as i32;
    let bits = u16_at(28).ok_or_else(header)?;
    let compression = u32_at(30).ok_or_else(header)?;
    if width <= 0 || height == 0 || bits != 32 || !(compression == 0 || compression == 3) {
        return Err(bad("not a 32-bit image"));
    }
    if compression == 3 {
        let masks = (u32_at(54), u32_at(58), u32_at(62));
        if masks != (Some(0x00ff_0000), Some(0x0000_ff00), Some(0x0000_00ff)) {
            return Err(bad("unknown channel order"));
        }
    }
    let (width, rows) = (width as usize, height.unsigned_abs() as usize);
    let stride = width * 4;
    let size = stride
        .checked_mul(rows)
        .ok_or_else(|| bad("image too large"))?;
    let pixels = offset
        .checked_add(size)
        .and_then(|end| bytes.get(offset..end))
        .ok_or_else(|| bad("short pixel data"))?;
    let mut bgra = Vec::with_capacity(size);
    for row in 0..rows {
        // A positive height stores the bottom row first.
        let source = if height > 0 { rows - 1 - row } else { row };
        bgra.extend_from_slice(&pixels[source * stride..(source + 1) * stride]);
    }
    // The renderer leaves alpha unset in some forms, and a page is opaque.
    for alpha in bgra.iter_mut().skip(3).step_by(4) {
        *alpha = 255;
    }
    Ok(viewer_core::PdfPageImage {
        width: width as u32,
        height: rows as u32,
        bgra,
    })
}

#[cfg(target_os = "windows")]
mod engine {
    use std::cell::Cell;
    use std::path::Path;

    use viewer_core::{PdfInfo, PdfPageImage, PdfPageSize};
    use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
    use windows::Graphics::Imaging::BitmapEncoder;
    use windows::Storage::StorageFile;
    use windows::Storage::Streams::{DataReader, InMemoryRandomAccessStream};
    use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
    use windows::core::HSTRING;

    use super::decode_bmp;

    fn code(error: windows::core::Error) -> String {
        // The system's message is in the language of Windows, so only the code goes into the viewer's English text.
        format!("error {:#010x}", error.code().0)
    }

    /// An open PDF document. It stays on the thread that opened it.
    pub struct PdfFile {
        doc: PdfDocument,
        /// Pixels the renderer gives per pixel asked for, learned from the last page drawn.
        scale: Cell<f64>,
    }

    impl PdfFile {
        /// Opens a file, starting from what earlier files taught about the renderer's scale.
        pub fn open(path: &Path, scale: f64) -> Result<Self, String> {
            // A missing or unreadable file reports as such, before the renderer's own codes.
            std::fs::metadata(path).map_err(|error| match error.kind() {
                std::io::ErrorKind::NotFound => "file not found".to_string(),
                std::io::ErrorKind::PermissionDenied => {
                    "no permission to read the file".to_string()
                }
                _ => format!("cannot read the file ({error})"),
            })?;
            let path = std::path::absolute(path).map_err(|error| error.to_string())?;
            // SAFETY: joining the multithreaded apartment has no preconditions besides being on this thread, and a
            // thread that has joined one already keeps it.
            let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
            let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(path.as_os_str()))
                .and_then(|operation| operation.join())
                .map_err(code)?;
            let doc = PdfDocument::LoadFromFileAsync(&file)
                .and_then(|operation| operation.join())
                .map_err(code)?;
            Ok(Self {
                doc,
                scale: Cell::new(scale),
            })
        }

        pub fn scale(&self) -> f64 {
            self.scale.get()
        }

        pub fn info(&self) -> Result<PdfInfo, String> {
            let count = self.doc.PageCount().map_err(code)?;
            let pages = (0..count)
                .map(|index| {
                    let size = self.doc.GetPage(index)?.Size()?;
                    Ok(PdfPageSize {
                        width: size.Width,
                        height: size.Height,
                    })
                })
                .collect::<windows::core::Result<Vec<_>>>()
                .map_err(code)?;
            Ok(PdfInfo { pages })
        }

        pub fn render(&self, index: u32, width: u32) -> Result<PdfPageImage, String> {
            let page = self.doc.GetPage(index).map_err(code)?;
            let asked = ((f64::from(width) / self.scale.get()).round() as u32).max(1);
            let options = PdfPageRenderOptions::new().map_err(code)?;
            options.SetDestinationWidth(asked).map_err(code)?;
            options
                .SetBitmapEncoderId(BitmapEncoder::BmpEncoderId().map_err(code)?)
                .map_err(code)?;
            let stream = InMemoryRandomAccessStream::new().map_err(code)?;
            page.RenderWithOptionsToStreamAsync(&stream, &options)
                .and_then(|action| action.join())
                .map_err(code)?;
            let len = u32::try_from(stream.Size().map_err(code)?)
                .map_err(|_| "page image too large".to_string())?;
            let reader = stream
                .GetInputStreamAt(0)
                .and_then(|input| DataReader::CreateDataReader(&input))
                .map_err(code)?;
            reader
                .LoadAsync(len)
                .and_then(|operation| operation.join())
                .map_err(code)?;
            let mut bytes = vec![0; len as usize];
            reader.ReadBytes(&mut bytes).map_err(code)?;
            let image = decode_bmp(&bytes)?;
            if image.width > 0 {
                self.scale.set(f64::from(image.width) / f64::from(asked));
            }
            Ok(image)
        }
    }
}

#[cfg(target_os = "windows")]
mod worker {
    use std::io::{self, BufReader, BufWriter};
    use std::path::PathBuf;
    use std::sync::{Arc, Condvar, Mutex};
    use std::thread;

    use viewer_core::{PdfReply, PdfRequest, read_frame, write_frame};

    use crate::engine::PdfFile;

    /// Requests waiting to be done. Only the latest of each kind counts.
    #[derive(Default)]
    struct Mailbox {
        open: Option<(u64, PathBuf)>,
        draw: Option<(u64, u32, Vec<u32>)>,
    }

    impl Mailbox {
        fn has_work(&self) -> bool {
            self.open.is_some() || self.draw.is_some()
        }
    }

    type Slot = Arc<(Mutex<Mailbox>, Condvar)>;

    /// Reads requests until stdin closes, then ends the process: the viewer is gone.
    fn read_requests(slot: &Slot) {
        let mut input = BufReader::new(io::stdin().lock());
        loop {
            let request = match read_frame(&mut input) {
                Ok(Some(body)) => PdfRequest::decode(&body),
                Ok(None) | Err(_) => std::process::exit(0),
            };
            let Ok(request) = request else {
                std::process::exit(2);
            };
            let (lock, wake) = &**slot;
            let Ok(mut mailbox) = lock.lock() else {
                std::process::exit(2);
            };
            match request {
                PdfRequest::Open { doc, path } => {
                    mailbox.open = Some((doc, path));
                    mailbox.draw = None;
                }
                PdfRequest::Draw { doc, width, pages } => mailbox.draw = Some((doc, width, pages)),
            }
            wake.notify_one();
        }
    }

    fn send(reply: &PdfReply) {
        let mut out = BufWriter::new(io::stdout().lock());
        if write_frame(&mut out, &reply.encode()).is_err() {
            std::process::exit(0);
        }
    }

    pub fn run() {
        let slot: Slot = Arc::default();
        let reader = slot.clone();
        thread::spawn(move || read_requests(&reader));
        let (lock, wake) = &*slot;
        // The document the viewer shows, and what the renderer taught about its scale.
        let mut current: Option<(u64, PathBuf)> = None;
        let mut scale = 1.;
        loop {
            let (open, draw) = {
                let Ok(mut mailbox) = lock.lock() else {
                    return;
                };
                while !mailbox.has_work() {
                    mailbox = match wake.wait(mailbox) {
                        Ok(mailbox) => mailbox,
                        Err(_) => return,
                    };
                }
                match mailbox.open.take() {
                    Some(open) => (Some(open), None),
                    None => (None, mailbox.draw.take()),
                }
            };
            if let Some((doc, path)) = open {
                let reply = match PdfFile::open(&path, scale).and_then(|file| file.info()) {
                    Ok(info) => PdfReply::Info { doc, info },
                    Err(message) => PdfReply::Failed {
                        doc,
                        page: None,
                        message,
                    },
                };
                current = Some((doc, path));
                send(&reply);
                continue;
            }
            let Some((doc, width, pages)) = draw else {
                continue;
            };
            let Some(path) = current
                .as_ref()
                .filter(|(shown, _)| *shown == doc)
                .map(|(_, path)| path.clone())
            else {
                continue;
            };
            let file = match PdfFile::open(&path, scale) {
                Ok(file) => file,
                Err(message) => {
                    send(&PdfReply::Failed {
                        doc,
                        page: None,
                        message,
                    });
                    continue;
                }
            };
            for page in pages {
                if lock.lock().map_or(true, |mailbox| mailbox.has_work()) {
                    break;
                }
                let reply = match file.render(page, width) {
                    Ok(image) => PdfReply::Page {
                        doc,
                        page,
                        width,
                        image,
                    },
                    Err(message) => PdfReply::Failed {
                        doc,
                        page: Some(page),
                        message,
                    },
                };
                scale = file.scale();
                send(&reply);
            }
            // The file closes here, until the next request.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A BMP of `width` × `height` whose pixel at each place is given, stored bottom row first when `bottom_up`.
    fn bmp(
        width: u32,
        height: u32,
        bottom_up: bool,
        pixel: impl Fn(u32, u32) -> [u8; 4],
    ) -> Vec<u8> {
        let mut data = Vec::new();
        for row in 0..height {
            let y = if bottom_up { height - 1 - row } else { row };
            for x in 0..width {
                data.extend_from_slice(&pixel(x, y));
            }
        }
        let mut out = b"BM".to_vec();
        out.extend_from_slice(&(54 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&54u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&(width as i32).to_le_bytes());
        let stored = if bottom_up {
            height as i32
        } else {
            -(height as i32)
        };
        out.extend_from_slice(&stored.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&[0; 24]);
        out.extend_from_slice(&data);
        out
    }

    #[test]
    fn reads_rows_from_the_top_either_way_bmp_stores_them() {
        let pixel = |x: u32, y: u32| [x as u8, y as u8, 7, 0];
        for bottom_up in [true, false] {
            let image = decode_bmp(&bmp(3, 2, bottom_up, pixel)).unwrap();
            assert_eq!((image.width, image.height), (3, 2));
            // The first row is y = 0, and alpha is set.
            assert_eq!(&image.bgra[..8], &[0, 0, 7, 255, 1, 0, 7, 255]);
            assert_eq!(&image.bgra[12..16], &[0, 1, 7, 255]);
        }
    }

    #[test]
    fn refuses_what_is_not_a_32_bit_bmp() {
        assert!(decode_bmp(b"PNG").is_err());
        let mut short = bmp(2, 2, true, |_, _| [0; 4]);
        short.truncate(60);
        assert!(decode_bmp(&short).is_err());
        let mut bits24 = bmp(2, 2, true, |_, _| [0; 4]);
        bits24[28] = 24;
        assert!(decode_bmp(&bits24).is_err());
    }

    /// A PDF with a wide page whose left half is black, and a tall empty page.
    #[cfg(target_os = "windows")]
    pub(crate) fn sample_pdf() -> Vec<u8> {
        let content = "0 0 0 rg 0 0 100 100 re f";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 5 0 R >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 300] >>".to_string(),
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ),
        ];
        let mut out = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (ix, object) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{object}\nendobj\n", ix + 1));
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
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        ));
        out.into_bytes()
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn reads_page_sizes_and_draws_pages() {
        use engine::PdfFile;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("두 쪽.pdf");
        std::fs::write(&path, sample_pdf()).unwrap();
        let file = PdfFile::open(&path, 1.).unwrap();
        let info = file.info().unwrap();
        assert_eq!(info.pages.len(), 2);
        let (wide, tall) = (info.pages[0], info.pages[1]);
        assert!((wide.width / wide.height - 2.).abs() < 0.01, "{wide:?}");
        assert!((tall.height / tall.width - 3.).abs() < 0.01, "{tall:?}");
        // The second drawing knows how the renderer scales, so it comes out at the width asked for.
        for _ in 0..2 {
            let image = file.render(0, 400).unwrap();
            assert_eq!(image.bgra.len(), (image.width * image.height * 4) as usize);
            assert!((f64::from(image.width) / f64::from(image.height) - 2.).abs() < 0.02);
        }
        let image = file.render(0, 400).unwrap();
        assert!(image.width.abs_diff(400) <= 1, "{}", image.width);
        let at = |x: u32, y: u32| &image.bgra[((y * image.width + x) * 4) as usize..][..4];
        let middle = image.height / 2;
        assert_eq!(at(image.width / 4, middle), &[0, 0, 0, 255]);
        assert_eq!(at(image.width * 3 / 4, middle), &[255, 255, 255, 255]);

        // A later opening starts at the learned scale, and once let go the file can be rewritten in place.
        let scale = file.scale();
        drop(file);
        let again = PdfFile::open(&path, scale).unwrap();
        assert!(again.render(0, 400).unwrap().width.abs_diff(400) <= 1);
        drop(again);
        std::fs::write(&path, sample_pdf()).unwrap();
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn reports_files_that_are_missing_or_not_pdf() {
        use engine::PdfFile;
        let dir = tempfile::tempdir().unwrap();
        let missing = PdfFile::open(&dir.path().join("없음.pdf"), 1.)
            .err()
            .unwrap();
        assert_eq!(missing, "file not found");
        let path = dir.path().join("a.pdf");
        std::fs::write(&path, b"not a pdf").unwrap();
        assert!(
            PdfFile::open(&path, 1.)
                .err()
                .unwrap()
                .starts_with("error 0x")
        );
    }
}
