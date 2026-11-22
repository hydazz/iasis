#[cfg(any(target_os = "macos", target_os = "linux"))]
mod gateway;

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [link, network, target, dns] => {
            gateway::run(link, network.parse()?, target.parse()?, dns.parse()?)
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
