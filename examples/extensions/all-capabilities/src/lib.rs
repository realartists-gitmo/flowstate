mod actions;
mod bindings;

struct Extension;

impl bindings::Guest for Extension {
  fn run(action_id: String) -> Result<(), String> {
    actions::run(&action_id)
  }
}

bindings::export!(Extension with_types_in bindings);
