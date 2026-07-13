use std::time::{SystemTime, UNIX_EPOCH};

use crate::bindings::flowstate::extension::host;

pub fn exercise() -> Result<(), String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?;
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(|error| error.to_string())?;
    let message = format!("time={}ms random={random:02x?}", now.as_millis());
    println!("all-capabilities stdout: {message}");
    eprintln!("all-capabilities stderr: captured output is working");
    host::set_status(&message);
    Ok(())
}
