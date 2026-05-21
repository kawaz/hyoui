pub(open) trait Observer {
  on_output(Self, Bytes) -> Bytes
  on_input(Self, Bytes) -> Bytes
  capture(Self) -> String
}
