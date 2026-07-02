use std::io::{self, Write};

const LEVELS: &[&[u8]] = &[b"INFO", b"DEBUG", b"WARN", b"ERROR", b"TRACE"];
const MODULES: &[&[u8]] = &[b"auth", b"db", b"api", b"cache", b"queue", b"scheduler", b"web", b"rpc", b"worker", b"health"];
const ACTIONS: &[&[u8]] = &[
    b"User login successful from {ip} ({email})",
    b"Query executed in {n}ms: SELECT * FROM users WHERE id = {n}",
    b"Rate limit approaching for key {key}",
    b"Failed login attempt from {ip} ({email})",
    b"Cache miss for key {key}",
    b"Job {key} completed in {n}ms",
    b"Connection pool depleted, waiting for release",
    b"Token refresh scheduled for {email}",
    b"Health check passed: {module} OK",
    b"Session expired for user {email}",
    b"Batch processing {n} records from {module}",
    b"Heartbeat received from node {ip}",
    b"Data migration chunk {n} committed",
    b"[CRITICAL_BUG_123] system integrity check failed on node {ip}",
    b"[security_audit] unauthorized access attempt detected from {ip}",
];

fn rand(seed: &mut u64) -> u64 {
    *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    *seed >> 33
}

fn pick<'a>(seed: &mut u64, slice: &[&'a [u8]]) -> &'a [u8] {
    slice[rand(seed) as usize % slice.len()]
}

fn fmt(seed: &mut u64, buf: &mut Vec<u8>, template: &[u8]) {
    let mut i = 0;
    while i < template.len() {
        if template[i] == b'{' {
            let end = template[i + 1..].iter().position(|&b| b == b'}').unwrap() + i + 1;
            let key = &template[i + 1..end];
            match key {
                b"ip" => {
                    for _ in 0..4 {
                        buf.extend_from_slice(rand(seed).to_string().as_bytes());
                        buf.push(b'.');
                    }
                    buf.pop();
                }
                b"email" => {
                    buf.extend_from_slice(b"user");
                    buf.extend_from_slice(rand(seed).to_string().as_bytes());
                    buf.push(b'@');
                    buf.extend_from_slice(pick(seed, &[b"example.com", b"test.org", b"corp.net"]));
                }
                b"key" => {
                    for _ in 0..8 {
                        buf.push(b"abcdef0123456789"[(rand(seed) as usize) % 16]);
                    }
                }
                b"n" => {
                    buf.extend_from_slice((rand(seed) % 9999 + 1).to_string().as_bytes());
                }
                b"module" => {
                    buf.extend_from_slice(pick(seed, MODULES));
                }
                _ => {}
            }
            i = end + 1;
        } else {
            buf.push(template[i]);
            i += 1;
        }
    }
}

fn main() {
    let target: u64 = 1 * 1024 * 1024 * 1024;
    let mut written: u64 = 0;
    let mut seed: u64 = 42;
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    let mut buf = Vec::with_capacity(65536);

    while written < target {
        buf.clear();
        let level = pick(&mut seed, LEVELS);
        let module = pick(&mut seed, MODULES);
        let action = pick(&mut seed, ACTIONS);

        buf.extend_from_slice(b"2024-01-15 10:30:45 ");
        buf.extend_from_slice(level);
        buf.push(b' ');
        buf.push(b'[');
        buf.extend_from_slice(module);
        buf.push(b']');
        buf.push(b' ');
        fmt(&mut seed, &mut buf, action);
        buf.push(b'\n');

        handle.write_all(&buf).unwrap();
        written += buf.len() as u64;
    }
    eprintln!("Generated {} bytes ({:.2} GB)", written, written as f64 / 1e9);
}
