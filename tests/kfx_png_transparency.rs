//! EPUB→KFX must flatten PNG transparency onto a white background.
//!
//! Kindle's KFX renderer composites image transparency over black, so
//! transparent areas render as black patches. Kindle Previewer avoids this
//! by flattening transparency at conversion time; boko must do the same.

use std::io::{Cursor, Write};

use boko::{Book, Format};
use zip::CompressionMethod;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

/// Encode a 2x1 RGBA PNG: left pixel fully transparent, right pixel opaque red.
fn make_transparent_png() -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut out, 2, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("write PNG header");
        writer
            .write_image_data(&[0, 0, 0, 0, 255, 0, 0, 255])
            .expect("write PNG data");
    }
    out
}

/// Build a minimal single-chapter EPUB containing the given PNG.
fn make_epub_with_png(png_data: &[u8]) -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut cursor);
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);

        zip.start_file("mimetype", stored).unwrap();
        zip.write_all(b"application/epub+zip").unwrap();

        zip.start_file("META-INF/container.xml", deflated).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container">
  <rootfiles>
    <rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/>
  </rootfiles>
</container>"#,
        )
        .unwrap();

        zip.start_file("OEBPS/content.opf", deflated).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" unique-identifier="uid" version="2.0">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>Transparency Test</dc:title>
    <dc:language>en</dc:language>
    <dc:identifier id="uid">urn:uuid:00000000-0000-0000-0000-000000000001</dc:identifier>
  </metadata>
  <manifest>
    <item id="chapter1" href="chapter1.xhtml" media-type="application/xhtml+xml"/>
    <item id="pic" href="images/pic.png" media-type="image/png"/>
  </manifest>
  <spine>
    <itemref idref="chapter1"/>
  </spine>
</package>"#,
        )
        .unwrap();

        zip.start_file("OEBPS/chapter1.xhtml", deflated).unwrap();
        zip.write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<html xmlns="http://www.w3.org/1999/xhtml">
<head><title>Chapter 1</title></head>
<body>
  <p>Before image.</p>
  <img src="images/pic.png" alt="test"/>
  <p>After image.</p>
</body>
</html>"#,
        )
        .unwrap();

        zip.start_file("OEBPS/images/pic.png", stored).unwrap();
        zip.write_all(png_data).unwrap();

        zip.finish().unwrap();
    }
    cursor.into_inner()
}

/// Extract every embedded PNG (signature → IEND) from a byte stream.
fn extract_pngs(data: &[u8]) -> Vec<&[u8]> {
    let mut pngs = Vec::new();
    let mut search_from = 0;
    while let Some(rel) = data[search_from..]
        .windows(PNG_SIGNATURE.len())
        .position(|w| w == PNG_SIGNATURE)
    {
        let start = search_from + rel;
        // Walk chunks: len(4) + type(4) + data + crc(4), starting after signature
        let mut i = start + 8;
        let mut end = None;
        while i + 8 <= data.len() {
            let len = u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
            let ctype = &data[i + 4..i + 8];
            let next = i + 12 + len;
            if next > data.len() {
                break;
            }
            if ctype == b"IEND" {
                end = Some(next);
                break;
            }
            i = next;
        }
        if let Some(end) = end {
            pngs.push(&data[start..end]);
            search_from = end;
        } else {
            search_from = start + 8;
        }
    }
    pngs
}

#[test]
fn test_kfx_export_flattens_png_transparency() {
    let epub = make_epub_with_png(&make_transparent_png());

    let mut book = Book::from_bytes(&epub, Format::Epub).expect("open EPUB");
    let mut output = Cursor::new(Vec::new());
    book.export(Format::Kfx, &mut output).expect("export KFX");
    let kfx = output.into_inner();

    let pngs = extract_pngs(&kfx);
    assert!(
        !pngs.is_empty(),
        "expected at least one embedded PNG in KFX output"
    );

    for png_data in pngs {
        let decoder = png::Decoder::new(Cursor::new(png_data));
        let mut reader = decoder.read_info().expect("decode embedded PNG");
        let info = reader.info();
        assert!(
            !matches!(
                info.color_type,
                png::ColorType::Rgba | png::ColorType::GrayscaleAlpha
            ),
            "embedded PNG still has an alpha channel (color type {:?})",
            info.color_type
        );
        assert!(
            info.trns.is_none(),
            "embedded PNG still has a tRNS transparency chunk"
        );

        // The formerly-transparent pixel must be white, the opaque pixel unchanged.
        let mut buf = vec![0u8; reader.output_buffer_size().expect("buffer size")];
        let frame = reader.next_frame(&mut buf).expect("read frame");
        assert_eq!(frame.color_type, png::ColorType::Rgb);
        assert_eq!(&buf[..6], &[255, 255, 255, 255, 0, 0]);
    }
}
