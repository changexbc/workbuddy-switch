use agent_studio_core::hub::Hub;
use std::io::Read;
fn main() {
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s).unwrap();
    let events: serde_json::Value = serde_json::from_str(&s).unwrap();
    let mut h = Hub::new();
    h.started = 1;
    h.ready = true;
    for e in events.as_array().unwrap() {
        h.ingest(e.clone());
    }
    println!("{}", h.snapshot());
}
