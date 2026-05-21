fn main {
  let observer = @observer.NullObserver::new()
  let agent = try {
    @hyoui_agent.Agent::new(observer)
  } catch {
    _ => {
      @ffi.proc_exit(1)
      panic()
    }
  }
  let status = agent.run()
  @ffi.proc_exit(status)
}
