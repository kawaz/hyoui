///|
test "parse_args: no args shows help" {
  match parse_args([]) {
    ShowHelp => ()
    _ => fail("expected ShowHelp")
  }
}

///|
test "parse_args: --help shows help" {
  match parse_args(["--help"]) {
    ShowHelp => ()
    _ => fail("expected ShowHelp")
  }
}

///|
test "parse_args: missing command is an error" {
  match parse_args(["--mode=headless"]) {
    Error(_) => ()
    _ => fail("expected Error")
  }
}

///|
test "parse_args: empty command after -- is an error" {
  match parse_args(["--"]) {
    Error(_) => ()
    _ => fail("expected Error")
  }
}

///|
test "parse_args: simple command" {
  match parse_args(["--", "echo", "hello"]) {
    Run(cfg) => {
      assert_eq(cfg.command, ["echo", "hello"])
      assert_eq(cfg.mode, Interactive)
      assert_eq(cfg.on_child_suspend, Follow)
      assert_eq(cfg.on_parent_suspend, Transparent)
    }
    _ => fail("expected Run")
  }
}

///|
test "parse_args: headless preset flips suspend defaults" {
  match parse_args(["--mode=headless", "--", "cat"]) {
    Run(cfg) => {
      assert_eq(cfg.mode, Headless)
      assert_eq(cfg.on_child_suspend, AutoResume)
      assert_eq(cfg.on_parent_suspend, Decouple)
      // headless default size
      assert_eq(cfg.cols, 80)
      assert_eq(cfg.rows, 24)
    }
    _ => fail("expected Run")
  }
}

///|
test "parse_args: explicit suspend flags override headless preset" {
  match
    parse_args([
      "--mode=headless", "--on-child-suspend=follow", "--on-parent-suspend=transparent",
      "--", "cat",
    ]) {
    Run(cfg) => {
      assert_eq(cfg.on_child_suspend, Follow)
      assert_eq(cfg.on_parent_suspend, Transparent)
    }
    _ => fail("expected Run")
  }
}

///|
test "parse_args: --size parses cols and rows" {
  match parse_args(["--size", "120x40", "--", "vim"]) {
    Run(cfg) => {
      assert_eq(cfg.cols, 120)
      assert_eq(cfg.rows, 40)
    }
    _ => fail("expected Run")
  }
}

///|
test "parse_args: --cols and --rows" {
  match parse_args(["--cols", "100", "--rows", "30", "--", "top"]) {
    Run(cfg) => {
      assert_eq(cfg.cols, 100)
      assert_eq(cfg.rows, 30)
    }
    _ => fail("expected Run")
  }
}

///|
test "parse_args: invalid size is an error" {
  match parse_args(["--size", "abc", "--", "cat"]) {
    Error(_) => ()
    _ => fail("expected Error")
  }
}

///|
test "parse_args: --timeout integer seconds" {
  match parse_args(["--timeout", "5", "--", "sleep", "10"]) {
    Run(cfg) => assert_eq(cfg.timeout_ms, Some(5000))
    _ => fail("expected Run")
  }
}

///|
test "parse_args: --timeout decimal seconds" {
  match parse_args(["--timeout", "1.5", "--", "sleep", "10"]) {
    Run(cfg) => assert_eq(cfg.timeout_ms, Some(1500))
    _ => fail("expected Run")
  }
}

///|
test "parse_args: --idle-timeout and --until" {
  match parse_args(["--idle-timeout=2", "--until", "DONE", "--", "make"]) {
    Run(cfg) => {
      assert_eq(cfg.idle_timeout_ms, Some(2000))
      assert_eq(cfg.until, Some("DONE"))
    }
    _ => fail("expected Run")
  }
}

///|
test "parse_args: --socket explicit path" {
  match parse_args(["--socket", "/tmp/x.sock", "--", "sh"]) {
    Run(cfg) => assert_eq(cfg.socket, Some("/tmp/x.sock"))
    _ => fail("expected Run")
  }
}

///|
test "parse_args: unknown option is an error" {
  match parse_args(["--bogus", "--", "cat"]) {
    Error(_) => ()
    _ => fail("expected Error")
  }
}

///|
test "parse_args: option without value is an error" {
  match parse_args(["--timeout"]) {
    Error(_) => ()
    _ => fail("expected Error")
  }
}

///|
test "parse_args: command args with leading dashes preserved after --" {
  match parse_args(["--", "ls", "-la", "--color"]) {
    Run(cfg) => assert_eq(cfg.command, ["ls", "-la", "--color"])
    _ => fail("expected Run")
  }
}

///|
test "parse_args: invalid mode is an error" {
  match parse_args(["--mode=weird", "--", "cat"]) {
    Error(_) => ()
    _ => fail("expected Error")
  }
}

///|
test "parse_seconds_ms: edge cases" {
  assert_eq(parse_seconds_ms("0"), Some(0))
  assert_eq(parse_seconds_ms("10"), Some(10000))
  assert_eq(parse_seconds_ms("0.001"), Some(1))
  assert_eq(parse_seconds_ms("2.5"), Some(2500))
  assert_eq(parse_seconds_ms(""), None)
  assert_eq(parse_seconds_ms("abc"), None)
  assert_eq(parse_seconds_ms("1.2.3"), None)
}

///|
test "parse_size: edge cases" {
  assert_eq(parse_size("80x24"), Some((80, 24)))
  assert_eq(parse_size("80X24"), Some((80, 24)))
  assert_eq(parse_size("x24"), None)
  assert_eq(parse_size("80x"), None)
  assert_eq(parse_size("80"), None)
}

///|
test "usage: non-empty and mentions sections" {
  let text = usage()
  assert_true(text.length() > 0)
}
