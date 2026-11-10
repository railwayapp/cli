use super::Parser;

#[test]
fn hyperlink_destinations_survive_chunking_styles_and_wide_characters() {
    let mut parser = Parser::new(4, 8, 10);
    let output =
        "\x1b]8;id=docs;https://example.com/a;b?x=1\x1b\\文e\u{301}\x1b[0m docs\x1b]8;;\x07!";
    for chunk in output.as_bytes().chunks(3) {
        parser.process(chunk);
    }
    for col in 0..8 {
        assert_eq!(
            parser.screen().cell(0, col).unwrap().hyperlink(),
            Some("https://example.com/a;b?x=1"),
            "column {col}"
        );
    }
    assert_eq!(parser.screen().cell(1, 0).unwrap().contents(), "!");
    assert_eq!(parser.screen().cell(1, 0).unwrap().hyperlink(), None);
}

#[test]
fn overwritten_and_erased_cells_lose_their_links() {
    let mut parser = Parser::new(3, 20, 10);
    parser.process(b"\x1b]8;;https://example.com\x07linked\x1b]8;;\x07\rX\x1b[2C\x1b[K");
    assert_eq!(parser.screen().cell(0, 0).unwrap().hyperlink(), None);
    assert_eq!(
        parser.screen().cell(0, 1).unwrap().hyperlink(),
        Some("https://example.com")
    );
    assert_eq!(parser.screen().cell(0, 3).unwrap().hyperlink(), None);
    parser.process(b"\x1b[2J");
    assert_eq!(parser.screen().cell(0, 1).unwrap().hyperlink(), None);
}

#[test]
fn hyperlinks_follow_text_through_scrollback_resize_and_alternate_screen() {
    let mut parser = Parser::new(3, 20, 10);
    parser.process(
        b"\x1b]8;;https://example.com/history\x07history\x1b]8;;\x07\r\nsecond\r\nthird\r\nfourth",
    );
    assert_eq!(parser.screen().cell(0, 0).unwrap().hyperlink(), None);
    parser.screen_mut().set_scrollback(1);
    assert_eq!(parser.screen().cell(0, 0).unwrap().contents(), "h");
    assert_eq!(
        parser.screen().cell(0, 0).unwrap().hyperlink(),
        Some("https://example.com/history")
    );
    parser.screen_mut().set_size(3, 25);
    assert_eq!(
        parser.screen().cell(0, 0).unwrap().hyperlink(),
        Some("https://example.com/history")
    );
    parser.process(b"\x1b[?1049hnew screen");
    assert_eq!(parser.screen().cell(0, 0).unwrap().hyperlink(), None);
    parser.process(b"\x1b[?1049l");
    parser.screen_mut().set_scrollback(1);
    assert_eq!(
        parser.screen().cell(0, 0).unwrap().hyperlink(),
        Some("https://example.com/history")
    );
}

#[test]
fn reset_and_invalid_destinations_end_the_active_link() {
    let mut parser = Parser::new(3, 20, 10);
    parser.process(b"\x1b]8;;https://example.com\x07old\x1bcnew");
    assert_eq!(parser.screen().cell(0, 0).unwrap().hyperlink(), None);
    parser.process(b"\x1b]8;;https://example.com\x07a\x1b]8;;\xff\x07b");
    assert_eq!(
        parser.screen().cell(0, 3).unwrap().hyperlink(),
        Some("https://example.com")
    );
    assert_eq!(parser.screen().cell(0, 4).unwrap().hyperlink(), None);
}
