//! PDF document facts and the messages between the viewer and its PDF worker, `tpui-pdf`. The worker opens files and draws
//! pages with the PDF renderer of Windows in a process of its own, so the renderer's memory and threads end with it.
//!
//! Messages travel over the worker's standard input and output as frames: a little-endian `u32` body length, then the body.
//! A body starts with a tag byte; numbers are little-endian, and texts and pixel data carry a `u32` length.

use std::io::{self, Read, Write};
use std::path::PathBuf;

/// Size of a page in device-independent pixels, 1/96 of an inch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdfPageSize {
    pub width: f32,
    pub height: f32,
}

/// The pages of a PDF file.
#[derive(Clone, Debug, PartialEq)]
pub struct PdfInfo {
    pub pages: Vec<PdfPageSize>,
}

/// A page drawn as pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PdfPageImage {
    pub width: u32,
    pub height: u32,
    /// Four bytes per pixel in blue, green, red, alpha order, rows from the top, without padding.
    pub bgra: Vec<u8>,
}

/// What the viewer asks of the worker. `doc` names one reading of a file; a file read again gets a new one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PdfRequest {
    /// Read the page sizes of a file. Draw requests for other documents are dropped from then on.
    Open { doc: u64, path: PathBuf },
    /// Draw these pages at this pixel width, in order. It replaces a draw request not yet done.
    Draw {
        doc: u64,
        width: u32,
        pages: Vec<u32>,
    },
}

/// What the worker answers.
#[derive(Clone, Debug, PartialEq)]
pub enum PdfReply {
    Info {
        doc: u64,
        info: PdfInfo,
    },
    /// A drawn page. `width` is the width that was asked for; the image may differ from it by a pixel.
    Page {
        doc: u64,
        page: u32,
        width: u32,
        image: PdfPageImage,
    },
    /// Opening the file failed when `page` is `None`, or drawing that page failed.
    Failed {
        doc: u64,
        page: Option<u32>,
        message: String,
    },
}

/// Largest frame either side accepts: a very tall page at a large width stays far below it.
const MAX_FRAME: usize = 1 << 30;

const OPEN: u8 = 1;
const DRAW: u8 = 2;
const INFO: u8 = 11;
const PAGE: u8 = 12;
const FAILED: u8 = 13;

fn invalid(reason: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("bad PDF worker message: {reason}"),
    )
}

/// Writes one frame and flushes it.
pub fn write_frame(out: &mut impl Write, body: &[u8]) -> io::Result<()> {
    if body.len() > MAX_FRAME {
        return Err(invalid("frame too large"));
    }
    out.write_all(&(body.len() as u32).to_le_bytes())?;
    out.write_all(body)?;
    out.flush()
}

/// Reads one frame. `None` means the other side closed its end between frames.
pub fn read_frame(input: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut len = [0; 4];
    match input.read_exact(&mut len) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(invalid("frame too large"));
    }
    let mut body = vec![0; len];
    input.read_exact(&mut body)?;
    Ok(Some(body))
}

#[derive(Default)]
struct Encoder(Vec<u8>);

impl Encoder {
    fn u8(&mut self, value: u8) -> &mut Self {
        self.0.push(value);
        self
    }

    fn u32(&mut self, value: u32) -> &mut Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    fn u64(&mut self, value: u64) -> &mut Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    fn f32(&mut self, value: f32) -> &mut Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }

    fn bytes(&mut self, value: &[u8]) -> &mut Self {
        self.u32(value.len() as u32);
        self.0.extend_from_slice(value);
        self
    }
}

struct Decoder<'a> {
    body: &'a [u8],
    at: usize,
}

impl<'a> Decoder<'a> {
    fn take(&mut self, len: usize) -> io::Result<&'a [u8]> {
        let end = self
            .at
            .checked_add(len)
            .ok_or_else(|| invalid("length overflow"))?;
        let part = self
            .body
            .get(self.at..end)
            .ok_or_else(|| invalid("message ends early"))?;
        self.at = end;
        Ok(part)
    }

    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("four bytes"),
        ))
    }

    fn u64(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("eight bytes"),
        ))
    }

    fn f32(&mut self) -> io::Result<f32> {
        Ok(f32::from_le_bytes(
            self.take(4)?.try_into().expect("four bytes"),
        ))
    }

    fn bytes(&mut self) -> io::Result<&'a [u8]> {
        let len = self.u32()? as usize;
        self.take(len)
    }

    fn text(&mut self) -> io::Result<String> {
        String::from_utf8(self.bytes()?.to_vec()).map_err(|_| invalid("text is not UTF-8"))
    }

    fn finish(&self) -> io::Result<()> {
        if self.at == self.body.len() {
            Ok(())
        } else {
            Err(invalid("trailing bytes"))
        }
    }
}

impl PdfRequest {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Encoder::default();
        match self {
            Self::Open { doc, path } => {
                out.u8(OPEN)
                    .u64(*doc)
                    .bytes(path.to_string_lossy().as_bytes());
            }
            Self::Draw { doc, width, pages } => {
                out.u8(DRAW).u64(*doc).u32(*width).u32(pages.len() as u32);
                for page in pages {
                    out.u32(*page);
                }
            }
        }
        out.0
    }

    pub fn decode(body: &[u8]) -> io::Result<Self> {
        let mut input = Decoder { body, at: 0 };
        let request = match input.u8()? {
            OPEN => Self::Open {
                doc: input.u64()?,
                path: PathBuf::from(input.text()?),
            },
            DRAW => {
                let (doc, width, count) = (input.u64()?, input.u32()?, input.u32()?);
                let pages = (0..count).map(|_| input.u32()).collect::<io::Result<_>>()?;
                Self::Draw { doc, width, pages }
            }
            _ => return Err(invalid("unknown request")),
        };
        input.finish()?;
        Ok(request)
    }
}

impl PdfReply {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Encoder::default();
        match self {
            Self::Info { doc, info } => {
                out.u8(INFO).u64(*doc).u32(info.pages.len() as u32);
                for page in &info.pages {
                    out.f32(page.width).f32(page.height);
                }
            }
            Self::Page {
                doc,
                page,
                width,
                image,
            } => {
                out.u8(PAGE)
                    .u64(*doc)
                    .u32(*page)
                    .u32(*width)
                    .u32(image.width)
                    .u32(image.height)
                    .bytes(&image.bgra);
            }
            Self::Failed { doc, page, message } => {
                out.u8(FAILED).u64(*doc);
                match page {
                    Some(page) => out.u8(1).u32(*page),
                    None => out.u8(0),
                };
                out.bytes(message.as_bytes());
            }
        }
        out.0
    }

    pub fn decode(body: &[u8]) -> io::Result<Self> {
        let mut input = Decoder { body, at: 0 };
        let reply = match input.u8()? {
            INFO => {
                let (doc, count) = (input.u64()?, input.u32()?);
                let pages = (0..count)
                    .map(|_| {
                        Ok(PdfPageSize {
                            width: input.f32()?,
                            height: input.f32()?,
                        })
                    })
                    .collect::<io::Result<_>>()?;
                Self::Info {
                    doc,
                    info: PdfInfo { pages },
                }
            }
            PAGE => {
                let (doc, page, width) = (input.u64()?, input.u32()?, input.u32()?);
                let (image_width, image_height) = (input.u32()?, input.u32()?);
                let bgra = input.bytes()?;
                let expected = u64::from(image_width) * u64::from(image_height) * 4;
                if bgra.len() as u64 != expected {
                    return Err(invalid("pixel data does not match the image size"));
                }
                Self::Page {
                    doc,
                    page,
                    width,
                    image: PdfPageImage {
                        width: image_width,
                        height: image_height,
                        bgra: bgra.to_vec(),
                    },
                }
            }
            FAILED => {
                let doc = input.u64()?;
                let page = match input.u8()? {
                    0 => None,
                    1 => Some(input.u32()?),
                    _ => return Err(invalid("bad page flag")),
                };
                Self::Failed {
                    doc,
                    page,
                    message: input.text()?,
                }
            }
            _ => return Err(invalid("unknown reply")),
        };
        input.finish()?;
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn through_a_pipe<T>(body: Vec<u8>, decode: impl Fn(&[u8]) -> io::Result<T>) -> T {
        let mut pipe = Vec::new();
        write_frame(&mut pipe, &body).unwrap();
        let mut input = pipe.as_slice();
        let frame = read_frame(&mut input).unwrap().unwrap();
        assert!(read_frame(&mut input).unwrap().is_none());
        decode(&frame).unwrap()
    }

    #[test]
    fn carries_every_request() {
        for request in [
            PdfRequest::Open {
                doc: 7,
                path: PathBuf::from(r"C:\자료\특허 (1).pdf"),
            },
            PdfRequest::Draw {
                doc: 8,
                width: 1440,
                pages: vec![3, 2, 4],
            },
            PdfRequest::Draw {
                doc: 9,
                width: 1,
                pages: vec![],
            },
        ] {
            assert_eq!(
                through_a_pipe(request.encode(), PdfRequest::decode),
                request
            );
        }
    }

    #[test]
    fn carries_every_reply() {
        for reply in [
            PdfReply::Info {
                doc: 1,
                info: PdfInfo {
                    pages: vec![
                        PdfPageSize {
                            width: 793.6,
                            height: 1122.4,
                        },
                        PdfPageSize {
                            width: 1122.4,
                            height: 793.6,
                        },
                    ],
                },
            },
            PdfReply::Page {
                doc: 2,
                page: 5,
                width: 2,
                image: PdfPageImage {
                    width: 2,
                    height: 1,
                    bgra: vec![1, 2, 3, 255, 4, 5, 6, 255],
                },
            },
            PdfReply::Failed {
                doc: 3,
                page: None,
                message: "error 0x80048018".into(),
            },
            PdfReply::Failed {
                doc: 3,
                page: Some(9),
                message: "한글 이유".into(),
            },
        ] {
            assert_eq!(through_a_pipe(reply.encode(), PdfReply::decode), reply);
        }
    }

    #[test]
    fn refuses_damaged_messages() {
        let page = PdfReply::Page {
            doc: 2,
            page: 5,
            width: 2,
            image: PdfPageImage {
                width: 2,
                height: 1,
                bgra: vec![0; 8],
            },
        }
        .encode();
        assert!(PdfReply::decode(&page[..page.len() - 1]).is_err());
        let mut extra = page.clone();
        extra.push(0);
        assert!(PdfReply::decode(&extra).is_err());
        assert!(PdfReply::decode(&[99]).is_err());
        assert!(PdfRequest::decode(&[]).is_err());
        // Pixel data that does not fill the image.
        let short = PdfReply::Page {
            doc: 2,
            page: 5,
            width: 2,
            image: PdfPageImage {
                width: 3,
                height: 1,
                bgra: vec![0; 8],
            },
        }
        .encode();
        assert!(PdfReply::decode(&short).is_err());
        // A frame cut in its middle is an error; one cut before it starts is a clean end.
        let mut pipe = Vec::new();
        write_frame(&mut pipe, &page).unwrap();
        assert!(read_frame(&mut &pipe[..10]).is_err());
        assert!(read_frame(&mut &pipe[..0]).unwrap().is_none());
    }
}
