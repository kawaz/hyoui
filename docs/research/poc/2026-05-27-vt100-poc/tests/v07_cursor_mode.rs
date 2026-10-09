//! 検証 7: cursor / mode 状態
//! cursor_position / hide_cursor / application_keypad / application_cursor /
//! bracketed_paste / mouse_protocol_mode を一通り叩く。

#[test]
fn v07_cursor_and_mode_states() {
    let mut p = vt100::Parser::new(10, 40, 100);

    // 初期状態
    assert_eq!(p.screen().cursor_position(), (0, 0));
    assert!(!p.screen().hide_cursor());
    assert!(!p.screen().application_keypad());
    assert!(!p.screen().application_cursor());
    assert!(!p.screen().bracketed_paste());

    // hide cursor (DECTCEM): \x1b[?25l
    p.process(b"\x1b[?25l");
    assert!(p.screen().hide_cursor(), "should be hidden after ?25l");

    // show cursor: \x1b[?25h
    p.process(b"\x1b[?25h");
    assert!(!p.screen().hide_cursor(), "should be visible after ?25h");

    // application keypad: \x1b=
    p.process(b"\x1b=");
    assert!(p.screen().application_keypad(), "should be app keypad after ESC=");
    p.process(b"\x1b>");
    assert!(!p.screen().application_keypad(), "should be normal keypad after ESC>");

    // application cursor (DECCKM): \x1b[?1h
    p.process(b"\x1b[?1h");
    assert!(p.screen().application_cursor());
    p.process(b"\x1b[?1l");
    assert!(!p.screen().application_cursor());

    // bracketed paste: \x1b[?2004h
    p.process(b"\x1b[?2004h");
    assert!(p.screen().bracketed_paste());
    p.process(b"\x1b[?2004l");
    assert!(!p.screen().bracketed_paste());

    // mouse protocol mode
    use vt100::MouseProtocolMode as M;
    assert_eq!(p.screen().mouse_protocol_mode(), M::None);
    p.process(b"\x1b[?1000h"); // X11 mouse
    let mm = p.screen().mouse_protocol_mode();
    eprintln!("v07 after ?1000h: mode={mm:?}");
    p.process(b"\x1b[?1002h"); // button event
    let mm = p.screen().mouse_protocol_mode();
    eprintln!("v07 after ?1002h: mode={mm:?}");
    p.process(b"\x1b[?1000l\x1b[?1002l");
    assert_eq!(p.screen().mouse_protocol_mode(), M::None);

    // cursor positioning: \x1b[5;10H → row=4 col=9 (0-origin)
    p.process(b"\x1b[5;10H");
    assert_eq!(p.screen().cursor_position(), (4, 9));

    eprintln!("v07: ✓ all cursor/mode state APIs work");
}
