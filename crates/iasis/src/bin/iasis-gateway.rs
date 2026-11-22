#[cfg(target_os = "macos")]
mod gateway;

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let [link, network, target, dns] = args.as_slice() else {
        anyhow::bail!(
            "expected USB interface, IPv4 /24 network, USB registry ID and IPv4 DNS server"
        );
    };
    gateway::run(link, network.parse()?, target.parse()?, dns.parse()?)
}

#[cfg(not(target_os = "macos"))]
fn main() -> std::process::ExitCode {
    eprintln!("USB internet sharing requires macOS");
    std::process::ExitCode::FAILURE
}
