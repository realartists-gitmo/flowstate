use futures_channel::oneshot;
use js_sys::{Uint8Array, wasm_bindgen::JsCast as _};
use std::{cell::RefCell, rc::Rc};
use wasm_bindgen::{JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Event, HtmlInputElement};

pub(super) async fn pick_db8_file() -> Result<Option<(String, Vec<u8>)>, JsValue> {
  let document = web_sys::window()
    .ok_or_else(|| JsValue::from_str("browser window unavailable"))?
    .document()
    .ok_or_else(|| JsValue::from_str("browser document unavailable"))?;
  let input = document
    .create_element("input")?
    .dyn_into::<HtmlInputElement>()?;
  input.set_type("file");
  input.set_accept(".db8,application/octet-stream");

  let (sender, receiver) = oneshot::channel();
  let sender = Rc::new(RefCell::new(Some(sender)));
  let change_sender = sender.clone();
  let input_for_change = input.clone();
  let on_change = Closure::<dyn FnMut(Event)>::new(move |_| {
    let picked = input_for_change.files().and_then(|files| files.get(0));
    if let Some(sender) = change_sender.borrow_mut().take() {
      let _ = sender.send(picked);
    }
  });
  let cancel_sender = sender;
  let on_cancel = Closure::<dyn FnMut(Event)>::new(move |_| {
    if let Some(sender) = cancel_sender.borrow_mut().take() {
      let _ = sender.send(None);
    }
  });
  input.set_onchange(Some(on_change.as_ref().unchecked_ref()));
  input.set_oncancel(Some(on_cancel.as_ref().unchecked_ref()));
  input.click();

  let Some(file) = receiver
    .await
    .map_err(|_| JsValue::from_str("file picker was cancelled"))?
  else {
    return Ok(None);
  };
  let name = file.name();
  let buffer = JsFuture::from(file.array_buffer()).await?;
  let bytes = Uint8Array::new(&buffer).to_vec();
  drop(on_change);
  drop(on_cancel);
  Ok(Some((name, bytes)))
}
