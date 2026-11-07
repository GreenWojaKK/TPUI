//! The PDF worker as a program: its messages over stdin and stdout, and its end when stdin closes. This test also has
//! Cargo build `tpui-pdf` before the app's own tests, which start it from the build folder.

#![cfg(target_os = "windows")]

use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use viewer_core::{PdfReply, PdfRequest, read_frame, write_frame};

/// Two pages: 200 x 100 points with a black left half, and 100 x 300 points.
fn sample(dir: &Path) -> PathBuf {
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
    out.push_str("xref\n0 6\n0000000000 65535 f \n");
    for offset in offsets {
        out.push_str(&format!("{offset:010} 00000 n \n"));
    }
    out.push_str(&format!(
        "trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
    ));
    let path = dir.join("두 쪽.pdf");
    std::fs::write(&path, out).unwrap();
    path
}

#[test]
fn answers_sizes_and_pages_and_ends_when_its_input_closes() {
    let dir = tempfile::tempdir().unwrap();
    let path = sample(dir.path());
    let mut worker = Command::new(env!("CARGO_BIN_EXE_tpui-pdf"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = BufWriter::new(worker.stdin.take().unwrap());
    let mut output = BufReader::new(worker.stdout.take().unwrap());
    let mut ask = |request: PdfRequest| write_frame(&mut input, &request.encode()).unwrap();
    let mut answer = || PdfReply::decode(&read_frame(&mut output).unwrap().unwrap()).unwrap();

    ask(PdfRequest::Open {
        doc: 1,
        path: path.clone(),
    });
    let PdfReply::Info { doc: 1, info } = answer() else {
        panic!("expected sizes");
    };
    assert_eq!(info.pages.len(), 2);
    assert!((info.pages[0].width / info.pages[0].height - 2.).abs() < 0.01);

    ask(PdfRequest::Draw {
        doc: 1,
        width: 400,
        pages: vec![0],
    });
    let PdfReply::Page {
        doc: 1,
        page: 0,
        width: 400,
        image,
    } = answer()
    else {
        panic!("expected a page");
    };
    let at = |x: u32, y: u32| &image.bgra[((y * image.width + x) * 4) as usize..][..4];
    assert_eq!(at(image.width / 4, image.height / 2), &[0, 0, 0, 255]);
    assert_eq!(
        at(image.width * 3 / 4, image.height / 2),
        &[255, 255, 255, 255]
    );

    // Between requests the file is closed, so another program can rewrite it in place. The worker closes it just after
    // sending the last page, so the first try may come a moment early.
    let rewritten = (0..100).any(|_| {
        let done = std::fs::write(&path, b"%PDF-1.4\n").is_ok();
        if !done {
            std::thread::sleep(Duration::from_millis(10));
        }
        done
    });
    assert!(
        rewritten,
        "the PDF stayed locked after the worker finished with it"
    );

    ask(PdfRequest::Open {
        doc: 2,
        path: dir.path().join("없음.pdf"),
    });
    let PdfReply::Failed {
        doc: 2,
        page: None,
        message,
    } = answer()
    else {
        panic!("expected a failure");
    };
    assert_eq!(message, "file not found");

    drop(input);
    let until = Instant::now() + Duration::from_secs(5);
    while Instant::now() < until {
        if worker.try_wait().unwrap().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = worker.kill();
    let _ = worker.wait();
    panic!("the worker kept running after its input closed");
}
