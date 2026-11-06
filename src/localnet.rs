//! Cheap local connectivity check: is there a default route? Reads /proc/net/route
//! (IPv4) and /proc/net/ipv6_route; no packets are sent.
use std::fs;

pub fn has_default_route() -> bool {
    let v4 = fs::read_to_string("/proc/net/route")
        .map(|t| v4_default(&t))
        .unwrap_or(false);
    if v4 {
        return true;
    }
    fs::read_to_string("/proc/net/ipv6_route")
        .map(|t| v6_default(&t))
        .unwrap_or(false)
}

pub fn v4_default(table: &str) -> bool {
    table.lines().skip(1).any(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        // Iface Destination Gateway Flags ... ; default = destination 0 with RTF_UP (0x1)
        f.len() > 3
            && f[1] == "00000000"
            && u32::from_str_radix(f[3], 16).is_ok_and(|fl| fl & 1 != 0)
    })
}

pub fn v6_default(table: &str) -> bool {
    table.lines().any(|l| {
        let f: Vec<&str> = l.split_whitespace().collect();
        // dest(32) plen(2) ... metric refcnt use flags iface; default has dest all zeros, plen 00
        f.len() >= 10
            && f[0] == "00000000000000000000000000000000"
            && f[1] == "00"
            && f[9] != "lo"
            && u32::from_str_radix(f[8], 16).is_ok_and(|fl| fl & 0x0001 != 0 && fl & 0x0200 == 0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v4_routes() {
        let with = "Iface\tDestination\tGateway\tFlags\tRefCnt\nwlan0\t00000000\t0100A8C0\t0003\t0\nwlan0\t0000A8C0\t00000000\t0001\t0\n";
        assert!(v4_default(with));
        let without = "Iface\tDestination\tGateway\tFlags\nwlan0\t0000A8C0\t00000000\t0001\n";
        assert!(!v4_default(without));
        let down = "Iface\tDestination\tGateway\tFlags\nwlan0\t00000000\t0100A8C0\t0002\n";
        assert!(!v4_default(down));
    }

    #[test]
    fn v6_routes() {
        let d = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 fe80000000000000aaaaaaaaaaaaaaaa 00000064 00000001 00000000 00000003 wlan0\n";
        assert!(v6_default(d));
        let lo = "00000000000000000000000000000000 00 00000000000000000000000000000000 00 00000000000000000000000000000000 ffffffff 00000001 00000000 00200200 lo\n";
        assert!(!v6_default(lo));
    }
}
