///|
/// hyoui — run a command inside a PTY as a transparent proxy.
fn main {
  // @env.args() includes argv[0] (the hyoui executable); the parser expects
  // only the arguments that follow it.
  let raw = @env.args()
  let argv : Array[String] = []
  for i = 1; i < raw.length(); i = i + 1 {
    argv.push(raw[i])
  }
  match @hyoui_agent.parse_args(argv) {
    ShowHelp => {
      println(@hyoui_agent.usage())
      @ffi.proc_exit(0)
    }
    Error(msg) => {
      println("hyoui: " + msg)
      println("")
      println(@hyoui_agent.usage())
      @ffi.proc_exit(1)
    }
    Run(config) => {
      let observer = @observer.NullObserver::new()
      let agent = @hyoui_agent.Agent::new(observer, config) catch {
        err => {
          println("hyoui: failed to start: " + err.to_string())
          @ffi.proc_exit(1)
          panic()
        }
      }
      let status = agent.run()
      @ffi.proc_exit(status)
    }
  }
}
