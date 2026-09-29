use crate::wasm_grants::GrantSet;

#[test]
fn vc_201_075_wasm_cannot_gain_host_network_or_write() {
    let host = GrantSet::host_baseline();
    let wasm = host.attenuate_for_wasm();
    assert!(wasm.allows("fs.read"));
    assert!(wasm.allows("clock"));
    assert!(!wasm.allows("net.loopback"));
    assert!(!wasm.allows("fs.write"));
}
