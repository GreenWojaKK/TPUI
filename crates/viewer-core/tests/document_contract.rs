//! Contracts of Markdown and JSON documents, checked against files on disk and, for JSON, against serde_json.

use std::fs;

use serde_json::Value;
use viewer_core::{
    Document, DocumentKind, JsonDocument, JsonKind, JsonNodeId, MdBlockKind, MdMarker, ViewerError,
    read_document,
};

/// Whether a node holds the same value as serde_json read from the same text.
fn assert_same(doc: &JsonDocument, node: JsonNodeId, value: &Value) {
    match value {
        Value::Null => assert_eq!(doc.kind(node), JsonKind::Null),
        Value::Bool(flag) => {
            assert_eq!(doc.kind(node), JsonKind::Bool);
            assert_eq!(doc.raw(node), flag.to_string());
        }
        Value::Number(number) => {
            assert_eq!(doc.kind(node), JsonKind::Number);
            let parsed: f64 = doc.raw(node).parse().unwrap();
            assert_eq!(parsed, number.as_f64().unwrap(), "{}", doc.raw(node));
        }
        Value::String(text) => {
            assert_eq!(doc.kind(node), JsonKind::String);
            assert_eq!(doc.text(node), text.as_str());
        }
        Value::Array(items) => {
            assert_eq!(doc.kind(node), JsonKind::Array);
            assert_eq!(doc.child_count(node), items.len());
            for (ix, item) in items.iter().enumerate() {
                assert_same(doc, doc.child(node, ix).unwrap(), item);
            }
        }
        Value::Object(members) => {
            assert_eq!(doc.kind(node), JsonKind::Object);
            assert_eq!(doc.child_count(node), members.len());
            for ix in 0..doc.child_count(node) {
                let child = doc.child(node, ix).unwrap();
                let key = doc.key(child).unwrap();
                assert_same(doc, child, &members[key.as_ref()]);
            }
        }
    }
}

#[test]
fn reads_the_same_values_as_serde_json() {
    for text in [
        "null",
        "true",
        "-0.5e-3",
        r#""\"\\\/\b\f\n\r\t\u0041\uD83D\uDE00가""#,
        "[]",
        "{}",
        " [ 1 , [ 2 , [ 3 ] ] , { } ] ",
        r#"{"이름":"홍길동","나이":30,"주소":{"시":"서울","동":null},"점수":[1.5,2,true,false]}"#,
        "{\n\t\"줄\": [\r\n1\r\n]\r\n}\n",
    ] {
        let doc = JsonDocument::parse(text.to_string()).unwrap();
        let value: Value = serde_json::from_str(text).unwrap();
        assert_same(&doc, doc.root(), &value);
    }
}

#[test]
fn accepts_and_rejects_the_same_texts_as_serde_json() {
    for text in [
        "",
        " ",
        "[",
        "]",
        "{",
        "}",
        "[1,]",
        "[,1]",
        "{\"a\":1,}",
        "{,}",
        "{\"a\"}",
        "{\"a\":}",
        "{a:1}",
        "{1:1}",
        "tru",
        "truee",
        "nul",
        "+1",
        "01",
        "-01",
        "1.",
        ".5",
        "1e",
        "1e+",
        "--1",
        "0x10",
        "\"",
        "\"\\\"",
        "\"\\q\"",
        "\"\\u12G4\"",
        "\"\t\"",
        "[1 2]",
        "{\"a\":1 \"b\":2}",
        "1 2",
        "[]]",
        "{}{}",
        "// x\n1",
        "'a'",
        "NaN",
        "Infinity",
        "-",
        "1.5.5",
        "1e5e5",
    ] {
        assert_eq!(
            JsonDocument::parse(text.to_string()).is_ok(),
            serde_json::from_str::<Value>(text).is_ok(),
            "{text:?}"
        );
    }
}

fn write_and_read(name: &str, content: &[u8], kind: DocumentKind) -> Result<Document, ViewerError> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    fs::write(&path, content).unwrap();
    let document = read_document(&path, kind);
    // Reading never changes the file.
    assert_eq!(fs::read(&path).unwrap(), content);
    document
}

#[test]
fn json_files_keep_what_serde_json_would_lose() {
    let Ok(Document::Json(doc)) = write_and_read(
        "a.json",
        br#"{"b":12345678901234567890123,"a":1.10,"b":1e2}"#,
        DocumentKind::Json,
    ) else {
        panic!("expected JSON");
    };
    let root = doc.root();
    let members: Vec<(String, String)> = (0..doc.child_count(root))
        .map(|ix| {
            let child = doc.child(root, ix).unwrap();
            (
                doc.key(child).unwrap().into_owned(),
                doc.raw(child).to_string(),
            )
        })
        .collect();
    assert_eq!(
        members,
        [
            ("b".to_string(), "12345678901234567890123".to_string()),
            ("a".to_string(), "1.10".to_string()),
            ("b".to_string(), "1e2".to_string()),
        ]
    );
}

#[test]
fn markdown_files_become_blocks_in_reading_order() {
    let source = "\u{feff}# 제목\r\n\r\n문단 **굵게**\r\n\r\n- 항목\r\n\r\n```go\r\nfunc main() {}\r\n```\r\n";
    let Ok(Document::Markdown(doc)) =
        write_and_read("a.md", source.as_bytes(), DocumentKind::Markdown)
    else {
        panic!("expected Markdown");
    };
    assert_eq!(doc.blocks.len(), 4);
    assert!(matches!(
        doc.blocks[0].kind,
        MdBlockKind::Heading { level: 1, .. }
    ));
    assert_eq!(doc.blocks[2].marker, Some(MdMarker::Bullet));
    let MdBlockKind::Code { language, text } = &doc.blocks[3].kind else {
        panic!("expected code");
    };
    assert_eq!(language.as_deref(), Some("go"));
    assert_eq!(&*text.text, "func main() {}");
    let (start, end) = doc.full_range().unwrap();
    assert_eq!(
        doc.text_between(start, end),
        "제목\n\n문단 굵게\n\n항목\n\nfunc main() {}"
    );
}

#[test]
fn reports_files_that_cannot_be_read_as_documents() {
    assert!(matches!(
        write_and_read("a.md", b"\xea\xb0", DocumentKind::Markdown),
        Err(ViewerError::InvalidText { offset: 0 })
    ));
    assert!(matches!(
        write_and_read("a.json", b"{\n}\n,", DocumentKind::Json),
        Err(ViewerError::InvalidJson {
            line: 3,
            column: 1,
            ..
        })
    ));
}
