use std::time::{Duration, Instant};
fn main() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let url = std::env::args().nth(1).expect("url");
    let subscribe = std::env::args().nth(2);
    let (mut ws, response) = tungstenite::connect(url.as_str()).expect("connect");
    println!("status {}", response.status());
    if let Some(s) = subscribe {
        ws.send(tungstenite::Message::Text(s.into())).unwrap();
    }
    let start = Instant::now();
    let mut n = 0;
    while start.elapsed() < Duration::from_secs(4) && n < 6 {
        let msg = ws.read().expect("read");
        if let tungstenite::Message::Text(t) = msg {
            let t = t.as_str();
            println!("{}", &t[..t.len().min(400)]);
            n += 1;
        }
    }
}
