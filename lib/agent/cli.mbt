///| Operating mode for the agent.
pub enum Mode {
  Interactive
  Headless
} derive(Eq, Show)

///| Behavior when the child process is suspended (STOPPED).
pub enum OnChildSuspend {
  /// Follow the child: the parent also stops (SIGSTOP raised on self).
  Follow
  /// Resume the child immediately by sending SIGCONT.
  AutoResume
} derive(Eq, Show)

///| Behavior when the parent process is suspended (receives SIGTSTP).
pub enum OnParentSuspend {
  /// Stop the child group first, then stop the parent.
  Transparent
  /// Stop only the parent; leave the child running.
  Decouple
} derive(Eq, Show)

///| Parsed CLI configuration.
pub struct Config {
  mode : Mode
  cols : Int
  rows : Int
  /// Overall timeout in milliseconds, or None if unset.
  timeout_ms : Int?
  /// Output idle timeout in milliseconds, or None if unset.
  idle_timeout_ms : Int?
  /// Substring pattern that, when seen in PTY output, terminates the child.
  until : String?
  /// Explicit socket path, or None to auto-generate.
  socket : String?
  on_child_suspend : OnChildSuspend
  on_parent_suspend : OnParentSuspend
  /// argv of the child command (argv[0] resolved against $PATH).
  command : Array[String]
}

///| Result of parsing argv.
pub enum ParseResult {
  /// Successfully parsed; run with this config.
  Run(Config)
  /// Display usage text and exit 0 (--help or no args).
  ShowHelp
  /// Parse error; display message + usage and exit 1.
  Error(String)
}

///| The usage text shown for --help / no args / errors.
pub fn usage() -> String {
  #|hyoui — run a command inside a PTY as a transparent proxy
  #|
  #|USAGE:
  #|    hyoui [options] -- cmd [args...]
  #|
  #|SUBCOMMANDS:
  #|    (none)
  #|
  #|OPTIONS:
  #|    --mode=interactive|headless   Operating mode (default: interactive)
  #|    --size COLSxROWS              Virtual screen size, e.g. 80x24 (headless)
  #|    --cols N                      Virtual screen columns (headless)
  #|    --rows M                      Virtual screen rows (headless)
  #|    --timeout SEC                 Overall timeout in seconds
  #|    --idle-timeout SEC            Output idle timeout in seconds
  #|    --until PATTERN               Terminate when PATTERN appears in output
  #|    --socket PATH                 Unix socket path for input injection
  #|    --on-child-suspend=follow|auto-resume
  #|                                  Action when the child is stopped
  #|                                  (default: follow; headless: auto-resume)
  #|    --on-parent-suspend=transparent|decouple
  #|                                  Action when the parent is stopped
  #|                                  (default: transparent; headless: decouple)
  #|    --help                        Show this help and exit
  #|
  #|ENVIRONMENT:
  #|    SHELL            Fallback command when none is given (legacy)
  #|    XDG_RUNTIME_DIR  Base directory for the auto-generated socket path
  #|    TMPDIR           Socket path base when XDG_RUNTIME_DIR is unset
}

///| Parse a seconds string (integer or decimal) into milliseconds.
/// Returns None on invalid input or negative values.
fn parse_seconds_ms(s : String) -> Int? {
  if s.is_empty() {
    return None
  }
  let mut int_part = 0
  let mut frac_part = 0
  let mut frac_digits = 0
  let mut seen_dot = false
  let mut any_digit = false
  for i = 0; i < s.length(); i = i + 1 {
    let c = s[i]
    if c == '.' {
      if seen_dot {
        return None
      }
      seen_dot = true
    } else if c >= '0' && c <= '9' {
      any_digit = true
      let d = c.to_int() - '0'.to_int()
      if not(seen_dot) {
        int_part = int_part * 10 + d
      } else if frac_digits < 3 {
        frac_part = frac_part * 10 + d
        frac_digits = frac_digits + 1
      }
      // extra fractional digits beyond millisecond precision are dropped
    } else {
      return None
    }
  }
  if not(any_digit) {
    return None
  }
  // scale frac_part to exactly 3 digits (milliseconds)
  for _i = frac_digits; _i < 3; _i = _i + 1 {
    frac_part = frac_part * 10
  }
  Some(int_part * 1000 + frac_part)
}

///| Parse a non-negative integer string. Returns None on invalid input.
fn parse_int(s : String) -> Int? {
  if s.is_empty() {
    return None
  }
  let mut value = 0
  for i = 0; i < s.length(); i = i + 1 {
    let c = s[i]
    if c >= '0' && c <= '9' {
      value = value * 10 + (c.to_int() - '0'.to_int())
    } else {
      return None
    }
  }
  Some(value)
}

///| Parse a "COLSxROWS" size string. Returns (cols, rows) or None.
fn parse_size(s : String) -> (Int, Int)? {
  let mut x_index = -1
  for i = 0; i < s.length(); i = i + 1 {
    if s[i] == 'x' || s[i] == 'X' {
      x_index = i
      break
    }
  }
  if x_index <= 0 || x_index >= s.length() - 1 {
    return None
  }
  let cols_str = s.substring(start=0, end=x_index)
  let rows_str = s.substring(start=x_index + 1, end=s.length())
  match (parse_int(cols_str), parse_int(rows_str)) {
    (Some(c), Some(r)) => Some((c, r))
    _ => None
  }
}

///| Split an argument of the form "--name=value" into (name, Some(value)),
/// or "--name" into (name, None).
fn split_eq(arg : String) -> (String, String?) {
  for i = 0; i < arg.length(); i = i + 1 {
    if arg[i] == '=' {
      return (
        arg.substring(start=0, end=i),
        Some(arg.substring(start=i + 1, end=arg.length())),
      )
    }
  }
  (arg, None)
}

///| Parse the agent CLI arguments.
///
/// `args` is the argument vector excluding argv[0] (the hyoui executable).
/// `--` separates hyoui options from the child command.
///
/// Design rationale: implemented as a pure function over Array[String] with no
/// FFI, so it can be exercised by whitebox tests without spawning processes.
pub fn parse_args(args : Array[String]) -> ParseResult {
  if args.is_empty() {
    return ShowHelp
  }
  // defaults
  let mut mode = Interactive
  let mut explicit_cols : Int? = None
  let mut explicit_rows : Int? = None
  let mut timeout_ms : Int? = None
  let mut idle_timeout_ms : Int? = None
  let mut until : String? = None
  let mut socket : String? = None
  let mut on_child_suspend : OnChildSuspend? = None
  let mut on_parent_suspend : OnParentSuspend? = None
  let command : Array[String] = []
  let mut i = 0
  let mut in_command = false
  while i < args.length() {
    let arg = args[i]
    if in_command {
      command.push(arg)
      i = i + 1
      continue
    }
    if arg == "--" {
      in_command = true
      i = i + 1
      continue
    }
    if arg == "--help" {
      return ShowHelp
    }
    let (name, inline_value) = split_eq(arg)
    // Helper: obtain the value for an option, either inline (--x=v) or the
    // following argument (--x v).
    let take_value : () -> String? = fn() {
      match inline_value {
        Some(v) => Some(v)
        None => {
          if i + 1 < args.length() {
            let v = args[i + 1]
            i = i + 1
            Some(v)
          } else {
            None
          }
        }
      }
    }
    match name {
      "--mode" =>
        match take_value() {
          Some("interactive") => mode = Interactive
          Some("headless") => mode = Headless
          Some(other) => return Error("invalid --mode value: " + other)
          None => return Error("--mode requires a value")
        }
      "--size" =>
        match take_value() {
          Some(v) =>
            match parse_size(v) {
              Some((c, r)) => {
                explicit_cols = Some(c)
                explicit_rows = Some(r)
              }
              None => return Error("invalid --size value (expected COLSxROWS): " + v)
            }
          None => return Error("--size requires a value")
        }
      "--cols" =>
        match take_value() {
          Some(v) =>
            match parse_int(v) {
              Some(c) => explicit_cols = Some(c)
              None => return Error("invalid --cols value: " + v)
            }
          None => return Error("--cols requires a value")
        }
      "--rows" =>
        match take_value() {
          Some(v) =>
            match parse_int(v) {
              Some(r) => explicit_rows = Some(r)
              None => return Error("invalid --rows value: " + v)
            }
          None => return Error("--rows requires a value")
        }
      "--timeout" =>
        match take_value() {
          Some(v) =>
            match parse_seconds_ms(v) {
              Some(ms) => timeout_ms = Some(ms)
              None => return Error("invalid --timeout value: " + v)
            }
          None => return Error("--timeout requires a value")
        }
      "--idle-timeout" =>
        match take_value() {
          Some(v) =>
            match parse_seconds_ms(v) {
              Some(ms) => idle_timeout_ms = Some(ms)
              None => return Error("invalid --idle-timeout value: " + v)
            }
          None => return Error("--idle-timeout requires a value")
        }
      "--until" =>
        match take_value() {
          Some(v) => until = Some(v)
          None => return Error("--until requires a value")
        }
      "--socket" =>
        match take_value() {
          Some(v) => socket = Some(v)
          None => return Error("--socket requires a value")
        }
      "--on-child-suspend" =>
        match take_value() {
          Some("follow") => on_child_suspend = Some(Follow)
          Some("auto-resume") => on_child_suspend = Some(AutoResume)
          Some(other) =>
            return Error("invalid --on-child-suspend value: " + other)
          None => return Error("--on-child-suspend requires a value")
        }
      "--on-parent-suspend" =>
        match take_value() {
          Some("transparent") => on_parent_suspend = Some(Transparent)
          Some("decouple") => on_parent_suspend = Some(Decouple)
          Some(other) =>
            return Error("invalid --on-parent-suspend value: " + other)
          None => return Error("--on-parent-suspend requires a value")
        }
      other => return Error("unknown option: " + other)
    }
    i = i + 1
  }
  if command.is_empty() {
    return Error("no command given (use `-- cmd [args...]`)")
  }
  // Mode-driven preset defaults for suspend behavior, unless overridden.
  let final_child_suspend = match on_child_suspend {
    Some(v) => v
    None =>
      match mode {
        Headless => AutoResume
        Interactive => Follow
      }
  }
  let final_parent_suspend = match on_parent_suspend {
    Some(v) => v
    None =>
      match mode {
        Headless => Decouple
        Interactive => Transparent
      }
  }
  // Virtual size: in headless, default to 80x24 when unspecified.
  let cols = match explicit_cols {
    Some(c) => c
    None => 80
  }
  let rows = match explicit_rows {
    Some(r) => r
    None => 24
  }
  Run(Config::{
    mode,
    cols,
    rows,
    timeout_ms,
    idle_timeout_ms,
    until,
    socket,
    on_child_suspend: final_child_suspend,
    on_parent_suspend: final_parent_suspend,
    command,
  })
}
