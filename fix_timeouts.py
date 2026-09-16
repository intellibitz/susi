import re

def process_file(path, replacements):
    with open(path, 'r') as f:
        code = f.read()
    for old, new in replacements:
        code = code.replace(old, new)
    with open(path, 'w') as f:
        f.write(code)

# 1. Main.rs
with open('src/main.rs', 'r') as f:
    code = f.read()
# Remove the setsockopt block
code = re.sub(r'let timeout_secs = cfg\.stdin_timeout_secs;[\s\S]*?\}', '', code)
with open('src/main.rs', 'w') as f:
    f.write(code)


# 2. task_manager.rs
with open('src/gawd/task_manager.rs', 'r') as f:
    code = f.read()
# Remove stall condition
stall_code = """                        if idle > thresh {
                            // Attempt atomic stall marking
                            if record
                                .status
                                .compare_exchange(
                                    TaskStatus::Running as u8,
                                    TaskStatus::Stalled as u8,
                                    Ordering::SeqCst,
                                    Ordering::Acquire,
                                )
                                .is_ok()
                            {
                                warn!(
                                    "[SwarmWatchdog] Task {} ({}) stalled (idle {}s > thresh {}s)",
                                    record.task_id, record.name, idle, thresh
                                );
                                if let Some(cancel) = mgr.cancel_map.get(&record.task_id) {
                                    cancel.store(true, Ordering::Release);
                                }
                                *record.result.write() = Some(format!("Stalled: {}s idle", idle));
                            }
                        }"""
code = code.replace(stall_code, "// Hardware limits only. Watchdog observes but does not artificially stall.")
# Change get_idle_threshold_ms to return u64::MAX
code = code.replace("10_000 // Reduced IDE timeout to 10s as requested", "u64::MAX // Hardware limits only (No software timeout)")
with open('src/gawd/task_manager.rs', 'w') as f:
    f.write(code)

# 3. gemi/engine.rs
with open('src/gemi/engine.rs', 'r') as f:
    code = f.read()
contention_code = """                    if wait_start.elapsed().as_millis() > 1000 {
                        return Err(EaiError::inference("Model substrate busy (Contention limit reached). Failing fast."));
                    }"""
code = code.replace(contention_code, "// Unlimited hardware wait (No artificial software contention timeout)")
with open('src/gemi/engine.rs', 'w') as f:
    f.write(code)

# 4. gmcp/client.rs
with open('src/gmcp/client.rs', 'r') as f:
    code = f.read()
code = code.replace(".timeout(std::time::Duration::from_millis(500))", "")
code = code.replace(".timeout(std::time::Duration::from_millis(1000))", "")
with open('src/gmcp/client.rs', 'w') as f:
    f.write(code)

# 5. gawd/agents.rs
with open('src/gawd/agents.rs', 'r') as f:
    code = f.read()
code = code.replace(".timeout(std::time::Duration::from_millis(50))", "")
code = code.replace(".timeout(std::time::Duration::from_millis(1000))", "")
with open('src/gawd/agents.rs', 'w') as f:
    f.write(code)

# 6. gawd/amas.rs
with open('src/gawd/amas.rs', 'r') as f:
    code = f.read()
code = code.replace("let _ = socket.set_read_timeout(Some(Duration::from_millis(100)));", "")
code = code.replace("let _ = socket.set_read_timeout(Some(Duration::from_millis(200)));", "")
code = code.replace("TcpStream::connect_timeout(\\n                                &std::net::SocketAddr::new(\\n                                    std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)),\\n                                    9090,\\n                                ),\\n                                Duration::from_millis(500),\\n                            )", "TcpStream::connect(std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)), 9090))")
code = code.replace("TcpStream::connect_timeout(\n                                &std::net::SocketAddr::new(\n                                    std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)),\n                                    9090,\n                                ),\n                                Duration::from_millis(500),\n                            )", "TcpStream::connect(std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)), 9090))")

with open('src/gawd/amas.rs', 'w') as f:
    f.write(code)

# 7. gemi/models.rs
with open('src/gemi/models.rs', 'r') as f:
    code = f.read()
code = code.replace(".timeout(Duration::from_secs(3600))", "")
code = code.replace(".timeout(Duration::from_secs(3)) // 3s fast timeout", "")
with open('src/gemi/models.rs', 'w') as f:
    f.write(code)

# 8. daemon/server.rs
with open('src/daemon/server.rs', 'r') as f:
    code = f.read()
code = code.replace("let _ = socket.set_read_timeout(Some(Duration::from_secs(1)));", "")
with open('src/daemon/server.rs', 'w') as f:
    f.write(code)

