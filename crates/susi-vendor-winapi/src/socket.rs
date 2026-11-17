//! Safe Windows socket-handle operations used by supervised service owners.

use std::os::windows::io::{AsRawSocket, AsSocket};

/// Prevent an existing live socket from being inherited by later children.
/// The caller still owns the socket; failure leaves that ownership unchanged.
// SAFETY: AsSocket lends a valid live handle for the complete native call.
#[allow(unsafe_code)]
pub fn forbid_inheritance(socket: &impl AsSocket) -> std::io::Result<()> {
    const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
    let socket = socket.as_socket();
    // SAFETY: SetHandleInformation only clears the borrowed socket's inherit
    // bit. It neither closes the handle nor accesses caller-provided memory.
    if unsafe {
        winapi::um::handleapi::SetHandleInformation(
            socket.as_raw_socket() as winapi::um::winnt::HANDLE,
            HANDLE_FLAG_INHERIT,
            0,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! The daemon's original live-child port-release regression at the OS boundary.

    // These tests assert real process and socket outcomes with isolated state.
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use std::net::{TcpListener, UdpSocket};
    use std::process::Stdio;
    use std::string::{String, ToString};
    use std::time::{Duration, Instant};
    use std::{format, vec::Vec};
    use susi_paths::console::background_command;

    #[test]
    #[ignore = "helper: plays a daemon that dies while its child lives"]
    #[allow(clippy::zombie_processes)] // The child must outlive this process.
    fn daemon_that_dies_with_a_child_alive() {
        let Some(report) = std::env::var_os("SUSI_SOCKFIX_REPORT") else {
            return;
        };
        let tcp = TcpListener::bind("127.0.0.1:0").unwrap();
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        let tcp_clone = tcp.try_clone().unwrap();
        let udp_clone = udp.try_clone().unwrap();
        super::forbid_inheritance(&tcp_clone).unwrap();
        super::forbid_inheritance(&udp_clone).unwrap();
        let child = background_command("ping")
            .args(["-n", "60", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        std::fs::write(
            report,
            format!(
                "{} {} {}",
                tcp.local_addr().unwrap().port(),
                udp.local_addr().unwrap().port(),
                child.id()
            ),
        )
        .unwrap();
        let _alive_until_exit = (tcp, udp, tcp_clone, udp_clone);
        std::process::exit(0);
    }

    #[test]
    fn a_dead_daemons_ports_are_free_while_its_child_lives() {
        let report = std::env::temp_dir().join(format!(
            "susi-runtime-dead-daemon-ports-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&report);
        let mut daemon = background_command(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "socket::tests::daemon_that_dies_with_a_child_alive",
            ])
            .env("SUSI_SOCKFIX_REPORT", &report)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(60);
        let status = loop {
            if let Some(status) = daemon.try_wait().unwrap() {
                break status;
            }
            if Instant::now() > deadline {
                let _ = daemon.kill();
                let _ = daemon.wait();
                panic!("the daemon stand-in did not exit within 60 s");
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(status.success(), "daemon stand-in failed: {status}");
        let line = std::fs::read_to_string(&report).unwrap();
        let _ = std::fs::remove_file(&report);
        let fields: Vec<u32> = line
            .split_whitespace()
            .map(|field| field.parse().unwrap())
            .collect();
        let (tcp_port, udp_port, child) = (fields[0] as u16, fields[1] as u16, fields[2]);
        struct Reap(u32);
        impl Drop for Reap {
            fn drop(&mut self) {
                let _ = background_command("taskkill")
                    .args(["/F", "/PID", &self.0.to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
        let _reap = Reap(child);
        let output = background_command("tasklist")
            .args(["/FI", &format!("PID eq {child}"), "/FO", "CSV", "/NH"])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(&format!("\"{child}\"")),
            "the child {child} must still run for this check to mean anything"
        );
        TcpListener::bind(("127.0.0.1", tcp_port)).unwrap_or_else(|error| {
            panic!("TCP {tcp_port} is still held after the daemon died (child {child}): {error}")
        });
        UdpSocket::bind(("127.0.0.1", udp_port)).unwrap_or_else(|error| {
            panic!("UDP {udp_port} is still held after the daemon died (child {child}): {error}")
        });
    }
}
