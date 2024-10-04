#[tokio::main]
async fn main() {
    std::env::set_var("RUST_LOG", "debug");
    env_logger::init();

    // Start looking for VRChat's OSC & OSCQuery services.
    oyasumivr_oscquery::client::init(
        "./lib/mdns-sidecar.exe", // The (relative) path to the mdns-sidecar.exe executable
    )
    .await
    .unwrap();

    // Wait a bit for the MDNS daemon to find the services
    tokio::time::sleep(tokio::time::Duration::from_millis(2000)).await;

    // Get the address of the VRChat OSC server
    let (host, port) = oyasumivr_oscquery::client::get_vrchat_osc_address()
        .await
        .unwrap();
    println!("VRChat OSC address: {}:{}", host, port);

    // Get the address of the VRChat OSCQuery server
    let (host, port) = oyasumivr_oscquery::client::get_vrchat_oscquery_address()
        .await
        .unwrap();
    println!("VRChat OSC Query address: {}:{}", host, port);
}
