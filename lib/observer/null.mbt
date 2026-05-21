pub struct NullObserver {
}

pub fn NullObserver::new() -> NullObserver {
  NullObserver::{  }
}

pub impl Observer for NullObserver with on_output(_self, data) { data }
pub impl Observer for NullObserver with on_input(_self, data) { data }
pub impl Observer for NullObserver with capture(_self) { "" }
