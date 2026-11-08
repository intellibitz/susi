//! Automatic host-port fallback (CEO decision 2026-11-08).
//!
//! The daemon binds six ports as one block: the five host-contract ports
//! (`susi_paths::ports::ALL`, 9090–9094) and swarm gossip (9095). Prometheus
//! listens on 9090 by default and Kafka brokers on 9092–9094, so a developer
//! machine often has one of them taken. When no offset is pinned
//! (`SusiConfig::explicit_port_offset` is `None`) and a foreign process
//! holds one of the six after the stale-susi reclaim, the daemon serves the
//! whole block at the first offset `k * 100` (`k` = 1..=20) where all six
//! are free. A pinned offset is bound exactly or the start refuses, as
//! before. The block never splits: every port moves by the same offset, so
//! the contract keeps its shape and a client needs one number.
//!
//! [`plan`] makes the decision from injected probes, so it is tested
//! without sockets; [`bind_probe`] is the probe the daemon uses, a real
//! bind on every address the daemon will bind.

use susi_paths::ports;

/// Transport of a block port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proto {
    Tcp,
    Udp,
}

impl Proto {
    /// Lower-case transport name for log lines.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Proto::Tcp => "tcp",
            Proto::Udp => "udp",
        }
    }
}

/// The six ports the daemon binds, at their canonical numbers.
pub const BLOCK: [(u16, Proto, &str); 6] = [
    (ports::GMCP, Proto::Tcp, "GMCP/MCP HTTP"),
    (ports::GEMI, Proto::Tcp, "GEMI HTTP"),
    (ports::UDP_DISCOVERY, Proto::Udp, "A2A UDP discovery"),
    (ports::GMCP_HTTP, Proto::Tcp, "GMCP HTTP alias"),
    (ports::A2A_HTTP, Proto::Tcp, "A2A HTTP"),
    (ports::GOSSIP, Proto::Udp, "swarm gossip"),
];

/// [`BLOCK`] without labels.
#[must_use]
pub fn block_ports() -> Vec<(u16, Proto)> {
    BLOCK
        .iter()
        .map(|(port, proto, _)| (*port, *proto))
        .collect()
}

/// Label of a canonical block port.
#[must_use]
pub fn label(port: u16) -> &'static str {
    BLOCK
        .iter()
        .find(|(p, _, _)| *p == port)
        .map_or("host port", |(_, _, name)| name)
}

/// The smallest offset `k * FALLBACK_STEP` (`k` = 0..=FALLBACK_BLOCKS) at
/// which every port in `base` is free, as `is_free(port, proto)` judges it;
/// `None` when no block in range is free. A block whose shifted port would
/// pass 65535 is never free.
pub fn pick_offset_block(
    base: &[(u16, Proto)],
    mut is_free: impl FnMut(u16, Proto) -> bool,
) -> Option<u16> {
    (0..=ports::FALLBACK_BLOCKS)
        .map(|k| k * ports::FALLBACK_STEP)
        .find(|offset| {
            base.iter().all(|(port, proto)| {
                port.checked_add(*offset)
                    .is_some_and(|shifted| is_free(shifted, *proto))
            })
        })
}

/// What the daemon binds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// The operator pinned this offset; bind it exactly (today's contract).
    Pinned(u16),
    /// Serve at `offset`; `held` lists the canonical ports a foreign process
    /// still held (empty when the canonical block was free).
    Serve {
        offset: u16,
        held: Vec<(u16, Proto)>,
    },
    /// The canonical block is held and no fallback block is free.
    Exhausted { held: Vec<(u16, Proto)> },
}

/// Decide the block. A pinned offset returns at once and probes nothing.
/// Otherwise every held canonical port goes through `reclaim` once (it
/// evicts only a stale susi process of this instance and says whether it
/// did); the canonical block is then probed again, and only ports still
/// held move the daemon to the first free block.
pub fn plan(
    pinned: Option<u16>,
    mut is_free: impl FnMut(u16, Proto) -> bool,
    mut reclaim: impl FnMut(u16) -> bool,
) -> Plan {
    if let Some(offset) = pinned {
        return Plan::Pinned(offset);
    }
    let base = block_ports();
    let mut held: Vec<(u16, Proto)> = base
        .iter()
        .copied()
        .filter(|(port, proto)| !is_free(*port, *proto))
        .collect();
    if !held.is_empty() {
        let mut reclaimed = false;
        for (port, _) in &held {
            reclaimed |= reclaim(*port);
        }
        if reclaimed {
            held.retain(|(port, proto)| !is_free(*port, *proto));
        }
    }
    if held.is_empty() {
        return Plan::Serve {
            offset: 0,
            held: Vec::new(),
        };
    }
    match pick_offset_block(&base, is_free) {
        Some(offset) => Plan::Serve { offset, held },
        None => Plan::Exhausted { held },
    }
}

/// The TCP addresses the daemon binds for `port`: `bind_address`, plus
/// loopback when `bind_address` is a specific non-loopback address (internal
/// callers dial 127.0.0.1; a wildcard already covers it).
#[must_use]
pub fn tcp_bind_addrs(bind_address: &str, port: u16) -> Vec<String> {
    let wildcard = matches!(bind_address, "0.0.0.0" | "::");
    let mut addrs = Vec::new();
    if !wildcard && !crate::tls::is_loopback(bind_address) {
        addrs.push(format!("127.0.0.1:{port}"));
    }
    addrs.push(format!("{bind_address}:{port}"));
    addrs
}

/// The UDP addresses a block port is bound on: A2A discovery binds
/// `bind_address`, gossip binds the wildcard; a port is free only when both
/// would bind.
fn udp_bind_addrs(bind_address: &str, port: u16) -> Vec<String> {
    let mut addrs = vec![format!("{bind_address}:{port}")];
    if bind_address != "0.0.0.0" {
        addrs.push(format!("0.0.0.0:{port}"));
    }
    addrs
}

/// Whether `port` would bind on every address the daemon uses for it. Each
/// probe socket is closed before the next, and before the real bind.
#[must_use]
pub fn bind_free(bind_address: &str, port: u16, proto: Proto) -> bool {
    match proto {
        Proto::Tcp => tcp_bind_addrs(bind_address, port)
            .iter()
            .all(|addr| std::net::TcpListener::bind(addr.as_str()).is_ok()),
        Proto::Udp => udp_bind_addrs(bind_address, port)
            .iter()
            .all(|addr| std::net::UdpSocket::bind(addr.as_str()).is_ok()),
    }
}

/// [`bind_free`] for one `bind_address`, as [`plan`]'s probe.
pub fn bind_probe(bind_address: &str) -> impl Fn(u16, Proto) -> bool + '_ {
    move |port, proto| bind_free(bind_address, port, proto)
}

/// `9190-9194 (gossip 9195)` for `offset`.
#[must_use]
pub fn describe_block(offset: u16) -> String {
    format!(
        "{}-{} (gossip {})",
        ports::GMCP.saturating_add(offset),
        ports::A2A_HTTP.saturating_add(offset),
        ports::GOSSIP.saturating_add(offset)
    )
}

#[cfg(test)]
// Test-only: assertions on fixed probe tables may unwrap and panic.
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn busy(ports: &[u16]) -> impl Fn(u16, Proto) -> bool + '_ {
        move |port, _| !ports.contains(&port)
    }

    #[test]
    fn canonical_block_free_serves_offset_zero() {
        assert_eq!(pick_offset_block(&block_ports(), |_, _| true), Some(0));
    }

    #[test]
    fn one_busy_canonical_port_moves_the_whole_block_by_100() {
        assert_eq!(pick_offset_block(&block_ports(), busy(&[9090])), Some(100));
        // Gossip counts as much as the contract ports.
        assert_eq!(pick_offset_block(&block_ports(), busy(&[9095])), Some(100));
    }

    #[test]
    fn busy_first_fallback_block_moves_to_200() {
        assert_eq!(
            pick_offset_block(&block_ports(), busy(&[9090, 9190])),
            Some(200)
        );
        assert_eq!(
            pick_offset_block(&block_ports(), busy(&[9092, 9194])),
            Some(200)
        );
    }

    #[test]
    fn nothing_free_in_range_is_none() {
        assert_eq!(pick_offset_block(&block_ports(), |_, _| false), None);
        // Only a block past k = 20 is free: still none.
        assert_eq!(
            pick_offset_block(&block_ports(), |port, _| port >= 9090 + 2100),
            None
        );
        // The last block in range is found.
        assert_eq!(
            pick_offset_block(&block_ports(), |port, _| port >= 9090 + 2000),
            Some(2000)
        );
    }

    #[test]
    fn the_probe_sees_the_transport_of_each_port() {
        let mut seen = BTreeSet::new();
        let _ = pick_offset_block(&block_ports(), |port, proto| {
            seen.insert((port, proto.label()));
            true
        });
        let udp: Vec<u16> = seen
            .iter()
            .filter(|(_, p)| *p == "udp")
            .map(|(port, _)| *port)
            .collect();
        assert_eq!(udp, vec![9092, 9095]);
        assert_eq!(seen.len(), 6);
    }

    #[test]
    fn a_block_past_the_port_space_is_never_free() {
        assert_eq!(
            pick_offset_block(&[(u16::MAX - 50, Proto::Tcp)], |_, _| true),
            Some(0)
        );
        assert_eq!(
            pick_offset_block(&[(u16::MAX - 50, Proto::Tcp)], |port, _| port
                != u16::MAX - 50),
            None,
            "the next block would pass 65535"
        );
    }

    #[test]
    fn a_pinned_offset_never_falls_back_and_probes_nothing() {
        let plan = plan(
            Some(300),
            |_, _| panic!("a pinned offset is not probed"),
            |_| panic!("a pinned offset reclaims through the bind retry, not here"),
        );
        assert_eq!(plan, Plan::Pinned(300));
        assert_eq!(
            super::plan(Some(0), |_, _| false, |_| false),
            Plan::Pinned(0)
        );
    }

    #[test]
    fn an_unpinned_daemon_falls_back_past_a_foreign_holder() {
        let mut reclaimed = Vec::new();
        let plan = plan(None, busy(&[9090]), |port| {
            reclaimed.push(port);
            false
        });
        assert_eq!(
            reclaimed,
            vec![9090],
            "the holder is offered to reclaim first"
        );
        assert_eq!(
            plan,
            Plan::Serve {
                offset: 100,
                held: vec![(9090, Proto::Tcp)]
            }
        );
        assert_eq!(
            super::plan(None, |_, _| true, |_| panic!("nothing held")),
            Plan::Serve {
                offset: 0,
                held: Vec::new()
            }
        );
    }

    #[test]
    fn a_reclaimed_stale_susi_holder_keeps_the_canonical_block() {
        let freed = std::cell::Cell::new(false);
        let plan = plan(
            None,
            |port, _| port != 9091 || freed.get(),
            |port| {
                assert_eq!(port, 9091);
                freed.set(true);
                true
            },
        );
        assert_eq!(
            plan,
            Plan::Serve {
                offset: 0,
                held: Vec::new()
            }
        );
    }

    #[test]
    fn no_free_block_is_exhausted_and_names_what_is_held() {
        let plan = plan(None, |port, _| port != 9092 && port < 9100, |_| false);
        assert_eq!(
            plan,
            Plan::Exhausted {
                held: vec![(9092, Proto::Udp)]
            }
        );
    }

    #[test]
    fn bind_probe_sees_a_held_tcp_port_and_a_free_one_quickly() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let held = listener.local_addr().unwrap().port();
        let free = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map(|a| a.port())
            .unwrap();
        let udp = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
        let udp_held = udp.local_addr().unwrap().port();
        let started = std::time::Instant::now();
        let probe = bind_probe("127.0.0.1");
        assert!(!probe(held, Proto::Tcp));
        assert!(probe(free, Proto::Tcp));
        assert!(!probe(udp_held, Proto::Udp));
        assert!(
            started.elapsed() < std::time::Duration::from_secs(1),
            "bind probes took {:?}",
            started.elapsed()
        );
        drop(listener);
        drop(udp);
    }

    #[test]
    fn tcp_bind_addrs_add_loopback_only_for_a_specific_remote_address() {
        assert_eq!(tcp_bind_addrs("127.0.0.1", 9090), vec!["127.0.0.1:9090"]);
        assert_eq!(tcp_bind_addrs("0.0.0.0", 9090), vec!["0.0.0.0:9090"]);
        assert_eq!(
            tcp_bind_addrs("192.168.1.5", 9190),
            vec!["127.0.0.1:9190", "192.168.1.5:9190"]
        );
    }

    #[test]
    fn block_description_names_contract_and_gossip_ports() {
        assert_eq!(describe_block(0), "9090-9094 (gossip 9095)");
        assert_eq!(describe_block(100), "9190-9194 (gossip 9195)");
        assert_eq!(label(9092), "A2A UDP discovery");
        assert_eq!(label(1), "host port");
    }
}
