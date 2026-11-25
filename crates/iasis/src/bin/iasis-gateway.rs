#[cfg(any(target_os = "macos", target_os = "linux"))]
mod gateway;

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn main() -> anyhow::Result<()> {
    iasis::logging::system();
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [link, network, target, dns] => gateway::run(
            gateway::Link::Interface {
                name: link,
                target: target.parse()?,
            },
            network.parse()?,
            dns.parse()?,
        ),
        // Frames from the agent on descriptor 3, for a Mac it reaches
        // through Debug USB.
        #[cfg(target_os = "linux")]
        [command, network, dns] if command == "tunnel" => {
            gateway::run(gateway::Link::tunnel()?, network.parse()?, dns.parse()?)
        }
        #[cfg(target_os = "linux")]
        [command, target] if command == "prepare" => gateway::prepare_usb(target.parse()?),
        _ => anyhow::bail!(
            "expected USB interface, IPv4 /24 network, USB attachment ID and IPv4 DNS server"
        ),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn main() -> std::process::ExitCode {
    eprintln!("USB internet sharing requires Linux or macOS");
    std::process::ExitCode::FAILURE
}
