#[tokio::main]
async fn main() {
    // Start looking for VRChat's OSC & OSCQuery services.
    //
    // This is also called by all of the available getter functions, 
    // however as it can take a few seconds for VRChat's services to be found, 
    // you may already want to call this function manually earlier in your program.
    oyasumivr_oscquery::client::init().await.unwrap();

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
