//! Read-only collection timing. Does not install hooks or save settings/resume.
use agent_studio_core::adapters::Collector;
fn main() {
    let home = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .expect("home path");
    let mut c = Collector::new(home).expect("settings");
    for round in 1..=2 {
        let t = std::time::Instant::now();
        c.poll();
        let collect = t.elapsed().as_millis();
        let t = std::time::Instant::now();
        let s = c.hub.snapshot();
        println!(
            "{}",
            serde_json::json!({"round":round,"collectMs":collect,"snapshotMs":t.elapsed().as_millis(),"sessions":s["sessions"].as_array().map(Vec::len),"events":s["events"].as_array().map(Vec::len),"bytes":s.to_string().len()})
        );
    }
}
