///|
test "NullObserver on_output passes data through" {
  let obs = NullObserver::new()
  let data = b"hello world"
  let result = Observer::on_output(obs, data)
  assert_eq(result, data)
}

///|
test "NullObserver on_input passes data through" {
  let obs = NullObserver::new()
  let data = b"user input"
  let result = Observer::on_input(obs, data)
  assert_eq(result, data)
}

///|
test "NullObserver capture returns empty string" {
  let obs = NullObserver::new()
  let result = Observer::capture(obs)
  assert_eq(result, "")
}
