#!/usr/bin/env python3
"""Generate a 1GB log-like file for benchmarking."""
import random
import sys
import time

random.seed(42)

LEVELS = ["INFO", "DEBUG", "WARN", "ERROR", "TRACE"]
MODULES = ["auth", "db", "api", "cache", "queue", "scheduler", "web", "rpc", "worker", "health"]
ACTIONS = [
    "User login successful from {ip} ({email})",
    "Query executed in {n}ms: SELECT * FROM users WHERE id = {n}",
    "Rate limit approaching for key {key}",
    "Failed login attempt from {ip} ({email})",
    "Cache miss for key {key}",
    "Job {key} completed in {n}ms",
    "Connection pool depleted, waiting for release",
    "Token refresh scheduled for {email}",
    "Health check passed: {module} OK",
    "Request routed to backend {key}",
    "Session expired for user {email}",
    "Batch processing {n} records from {module}",
    "Configuration reloaded from /etc/app/config.yaml",
    "Heartbeat received from node {ip}",
    "Data migration chunk {n} committed",
]

EMAIL_DOMAINS = ["example.com", "test.org", "corp.net", "mail.io", "service.co"]

def rand_ip():
    return f"{random.randint(1,255)}.{random.randint(0,255)}.{random.randint(0,255)}.{random.randint(1,254)}"

def rand_email():
    return f"user{random.randint(1,9999)}@{random.choice(EMAIL_DOMAINS)}"

def rand_key():
    return f"{random.choice('abcdef0123456789')}{random.choice('abcdef0123456789')}{random.choice('abcdef0123456789')}{random.choice('abcdef0123456789')}{random.choice('abcdef0123456789')}{random.choice('abcdef0123456789')}"

def rand_date():
    m = random.randint(1, 12)
    d = random.randint(1, 28)
    return f"2024-{m:02d}-{d:02d}"

def rand_time():
    return f"{random.randint(0,23):02d}:{random.randint(0,59):02d}:{random.randint(0,59):02d}"

def generate_line():
    level = random.choice(LEVELS)
    module = random.choice(MODULES)
    action = random.choice(ACTIONS)
    ip = rand_ip()
    email = rand_email()
    key = rand_key()
    n = random.randint(1, 9999)
    
    msg = action.format(ip=ip, email=email, key=key, n=n, module=module)
    
    # Occasionally inject some specific patterns for searching
    if random.random() < 0.001:
        msg += " [CRITICAL_BUG_123]"
    if random.random() < 0.002:
        msg += " [security_audit]"
    
    date = rand_date()
    time_str = rand_time()
    return f"{date} {time_str} {level} [{module}] {msg}"

def main():
    target_bytes = 1 * 1024 * 1024 * 1024  # 1 GB
    written = 0
    line_count = 0
    buf = []
    
    while written < target_bytes:
        line = generate_line() + "\n"
        buf.append(line)
        written += len(line.encode())
        line_count += 1
        
        if len(buf) >= 10000:
            sys.stdout.write("".join(buf))
            buf = []
    
    if buf:
        sys.stdout.write("".join(buf))
    
    print(f"Generated {line_count} lines ({written / 1e9:.2f} GB)", file=sys.stderr)

if __name__ == "__main__":
    main()
