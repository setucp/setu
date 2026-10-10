//! Listening TCP ports from `/proc/net/tcp` and `/proc/net/tcp6`, for the
//! `Inventory` report (provision `validate` checks the site port is free).

/// The `LISTEN` state (`0A`) in `/proc/net/tcp{,6}`.
const TCP_LISTEN: &str = "0A";

/// Ports in `LISTEN` in one `/proc/net/tcp`-format table (header line
/// skipped, malformed lines ignored).
pub fn parse_listening(table: &str) -> Vec<u16> {
    table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let mut cols = line.split_whitespace();
            let local = cols.nth(1)?;
            let state = cols.nth(1)?;
            if state != TCP_LISTEN {
                return None;
            }
            let (_, port_hex) = local.rsplit_once(':')?;
            u16::from_str_radix(port_hex, 16).ok()
        })
        .collect()
}

/// Every listening TCP port on this host (IPv4 and IPv6), sorted and
/// unique. A missing table (no IPv6) contributes nothing.
pub fn listening_ports() -> Vec<u16> {
    let mut ports: Vec<u16> = ["/proc/net/tcp", "/proc/net/tcp6"]
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .flat_map(|table| parse_listening(&table))
        .collect();
    ports.sort_unstable();
    ports.dedup();
    ports
}

#[cfg(test)]
mod tests {
    use super::*;

    const TCP: &str = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:4E20 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 1 1 0 100 0 0 10 0
   1: 0100007F:4E21 0100007F:9C40 01 00000000:00000000 00:00000000 00000000  1000        0 2 1 0 20 4 30 10 -1
   2: 00000000:0016 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 3 1 0 100 0 0 10 0
garbage
";
    const TCP6: &str = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 00000000000000000000000001000000:4E22 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 4 1 0 100 0 0 10 0
";

    #[test]
    fn parses_only_listen_entries() {
        assert_eq!(parse_listening(TCP), [20000, 22]);
        assert_eq!(parse_listening(TCP6), [20002]);
        assert_eq!(parse_listening(""), [] as [u16; 0]);
    }

    #[test]
    fn host_ports_are_sorted_and_unique() {
        let ports = listening_ports();
        let mut sorted = ports.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(ports, sorted);
    }
}
